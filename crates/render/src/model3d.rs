//! 3D model files read into one triangle mesh: positions, normals and
//! colours, in the file's own units and axes (right-handed, +Y up unless the
//! caller says Z). OBJ (with its MTL colours and diffuse textures), glTF/GLB
//! (node transforms, base colours, vertex colours and base-colour textures), FBX
//! (binary, 7.x: geometry, model transforms, material colours), USD
//! (USDA/USDC/USDZ: meshes with their transforms and display colours), STL
//! (binary and ASCII) and PLY (ASCII and binary, with vertex colours).
//!
//! Normals the file doesn't give are worked out from the faces: shared where
//! faces meet at less than 40°, so curved surfaces are smooth and edges sharp.

use std::path::Path;

/// A model as triangles.
#[derive(Clone, Debug, Default)]
pub struct Model {
    pub positions: Vec<[f32; 3]>,
    /// Per vertex; empty to be worked out.
    pub normals: Vec<[f32; 3]>,
    /// Per vertex, linear RGBA.
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    /// Per vertex: texture coordinates, which texture, which part.
    pub extra: Vec<VExtra>,
    /// The parts it's made of (materials, or objects), by name, for colouring one alone.
    pub parts: Vec<String>,
    /// Its textures, packed into one picture (see `Atlas`).
    pub atlas: Option<std::sync::Arc<Atlas>>,
    /// The file says its up axis is Z.
    pub z_up: bool,
}

/// What a vertex has besides its place, normal and colour.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VExtra {
    /// Texture coordinates (0–1 across the texture, from its top left; repeating outside).
    pub uv: [f32; 2],
    /// Which texture (−1: none).
    pub tex: i16,
    /// Which part (an index into `Model::parts`).
    pub part: u16,
}

impl VExtra {
    const NONE: VExtra = VExtra { uv: [0.0; 2], tex: -1, part: 0 };
}

/// A texture as read: RGBA8, sRGB.
#[derive(Clone, Debug)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A model's textures packed side by side in one RGBA8 (sRGB) picture, each
/// shrunk to at most `ATLAS_TEXTURE` across, and where each one went (x, y,
/// width, height as fractions of the atlas).
#[derive(Debug)]
pub struct Atlas {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub rects: Vec<[f32; 4]>,
    /// Changes with the pictures in it.
    pub id: u64,
}

/// The largest side a texture keeps in the atlas.
pub const ATLAS_TEXTURE: u32 = 1024;
/// The atlas's widest and tallest.
const ATLAS_MAX: u32 = 4096;

/// Light grey for parts without a colour.
const DEFAULT_COLOR: [f32; 4] = [0.6, 0.6, 0.6, 1.0];
/// Faces meeting at less than this share their normals.
const CREASE_DEG: f32 = 40.0;

/// Read the model in `path` (by its extension).
pub fn load(path: &Path) -> Result<Model, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let mut m = match ext.as_str() {
        "obj" => load_obj(path),
        "gltf" | "glb" => load_gltf(path),
        "fbx" => load_fbx(path),
        "usd" | "usda" | "usdc" | "usdz" => load_usd(path),
        "stl" => load_stl(path),
        "ply" => load_ply(path),
        _ => Err(format!("{ext} files aren't 3D models Reelforge reads")),
    }?;
    if m.indices.is_empty() {
        return Err("the file has no faces to show".into());
    }
    let n = m.positions.len();
    if m.indices.iter().any(|&i| i as usize >= n) {
        return Err("the file's faces point at vertices it doesn't have".into());
    }
    if m.colors.len() != n {
        m.colors = vec![DEFAULT_COLOR; n];
    }
    if m.extra.len() != n {
        m.extra = vec![VExtra::NONE; n];
    }
    if m.parts.is_empty() {
        m.parts.push("Model".into());
    }
    if m.normals.len() != n {
        m = with_normals(m);
    }
    Ok(m)
}

/// The index of the part called `name` (added if new).
fn part_index(parts: &mut Vec<String>, name: &str) -> u16 {
    let name = if name.trim().is_empty() { "Model" } else { name.trim() };
    match parts.iter().position(|p| p == name) {
        Some(i) => i as u16,
        None => {
            parts.push(name.to_string());
            (parts.len() - 1).min(u16::MAX as usize) as u16
        }
    }
}

/// Pack `pics` into one atlas: shelves, tallest first, each texture shrunk
/// to `ATLAS_TEXTURE` across (and all of them halved again until they fit),
/// with a pixel of its own edge round each so filtering doesn't bleed.
pub fn pack_atlas(pics: &[Picture]) -> Option<Atlas> {
    if pics.is_empty() {
        return None;
    }
    let mut limit = ATLAS_TEXTURE;
    loop {
        let sizes: Vec<(u32, u32)> = pics
            .iter()
            .map(|p| {
                let k = (limit as f32 / p.width.max(p.height).max(1) as f32).min(1.0);
                (((p.width as f32 * k).round() as u32).max(1), ((p.height as f32 * k).round() as u32).max(1))
            })
            .collect();
        let mut order: Vec<usize> = (0..pics.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(sizes[i].1));
        let width = ATLAS_MAX.min(sizes.iter().map(|s| s.0 + 2).sum::<u32>().max(sizes.iter().map(|s| s.0 + 2).max().unwrap_or(1)));
        let (mut x, mut y, mut shelf) = (0u32, 0u32, 0u32);
        let mut at = vec![(0u32, 0u32); pics.len()];
        let mut fits = true;
        for &i in &order {
            let (w, h) = (sizes[i].0 + 2, sizes[i].1 + 2);
            if w > width {
                fits = false;
                break;
            }
            if x + w > width {
                (x, y, shelf) = (0, y + shelf, 0);
            }
            at[i] = (x, y);
            x += w;
            shelf = shelf.max(h);
        }
        let height = y + shelf;
        if !fits || height > ATLAS_MAX {
            if limit <= 16 {
                return None;
            }
            limit /= 2;
            continue;
        }
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        let mut rects = Vec::with_capacity(pics.len());
        for (i, p) in pics.iter().enumerate() {
            let (w, h) = sizes[i];
            let (ox, oy) = (at[i].0 + 1, at[i].1 + 1);
            // Nearest-pixel shrink (box-filtered would be kinder; textures rarely need much).
            for yy in 0..h + 2 {
                for xx in 0..w + 2 {
                    let sx = ((xx.saturating_sub(1).min(w - 1) as f32 + 0.5) / w as f32 * p.width as f32) as u32;
                    let sy = ((yy.saturating_sub(1).min(h - 1) as f32 + 0.5) / h as f32 * p.height as f32) as u32;
                    let si = ((sy.min(p.height - 1) * p.width + sx.min(p.width - 1)) * 4) as usize;
                    let di = (((at[i].1 + yy) * width + at[i].0 + xx) * 4) as usize;
                    if si + 4 <= p.rgba.len() {
                        rgba[di..di + 4].copy_from_slice(&p.rgba[si..si + 4]);
                    }
                }
            }
            rects.push([ox as f32 / width as f32, oy as f32 / height as f32, w as f32 / width as f32, h as f32 / height as f32]);
        }
        let id = {
            use std::hash::{Hash, Hasher};
            let mut hh = std::collections::hash_map::DefaultHasher::new();
            (width, height).hash(&mut hh);
            rgba.iter().step_by(97).for_each(|b| b.hash(&mut hh));
            hh.finish()
        };
        return Some(Atlas { width, height, rgba, rects, id });
    }
}

/// A picture from any of the usual formats, as RGBA8.
fn read_picture(path: &Path) -> Option<Picture> {
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?.into_rgba8();
    Some(Picture { width: img.width(), height: img.height(), rgba: img.into_raw() })
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn unit(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt();
    if l > 1e-20 { [a[0] / l, a[1] / l, a[2] / l] } else { [0.0, 0.0, 1.0] }
}

/// Normals from the faces: each corner gets its own vertex, with the
/// (area-weighted) normals of the faces round its point that are within the
/// crease angle of its own face.
fn with_normals(m: Model) -> Model {
    let tris = m.indices.len() / 3;
    let face: Vec<[f32; 3]> = (0..tris)
        .map(|t| {
            let [a, b, c] = [0, 1, 2].map(|k| m.positions[m.indices[t * 3 + k] as usize]);
            cross(sub(b, a), sub(c, a))
        })
        .collect();
    // Faces round each point.
    let mut around: Vec<Vec<u32>> = vec![Vec::new(); m.positions.len()];
    for (k, &i) in m.indices.iter().enumerate() {
        around[i as usize].push((k / 3) as u32);
    }
    let cos = CREASE_DEG.to_radians().cos();
    let mut out = Model { z_up: m.z_up, ..Default::default() };
    for (k, &i) in m.indices.iter().enumerate() {
        let own = unit(face[k / 3]);
        let mut n = [0.0f32; 3];
        for &t in &around[i as usize] {
            let f = face[t as usize];
            if dot(unit(f), own) >= cos {
                n = [n[0] + f[0], n[1] + f[1], n[2] + f[2]];
            }
        }
        out.positions.push(m.positions[i as usize]);
        out.normals.push(unit(if dot(n, n) > 0.0 { n } else { own }));
        out.colors.push(m.colors[i as usize]);
        out.extra.push(m.extra.get(i as usize).copied().unwrap_or(VExtra::NONE));
        out.indices.push(k as u32);
    }
    out.parts = m.parts;
    out.atlas = m.atlas;
    out
}

/// A 4×4 matrix, column-major (as glTF and USD's row-vector layout transposed).
type M4 = [[f32; 4]; 4];
const I4: M4 = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

/// `a` after `b` (column-major: a · b).
fn mul(a: &M4, b: &M4) -> M4 {
    let mut r = [[0.0; 4]; 4];
    for c in 0..4 {
        for row in 0..4 {
            r[c][row] = (0..4).map(|k| a[k][row] * b[c][k]).sum();
        }
    }
    r
}
fn apply(m: &M4, p: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|r| m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r])
}
/// Normals under `m`: the inverse transpose of its 3×3 part (for non-uniform scales).
fn normal_matrix(m: &M4) -> [[f32; 3]; 3] {
    let a = |r: usize, c: usize| m[c][r];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0)) + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    let d = if det.abs() > 1e-20 { 1.0 / det } else { 1.0 };
    // Cofactors / det = inverse transpose.
    [
        [(a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) * d, -(a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0)) * d, (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0)) * d],
        [-(a(0, 1) * a(2, 2) - a(0, 2) * a(2, 1)) * d, (a(0, 0) * a(2, 2) - a(0, 2) * a(2, 0)) * d, -(a(0, 0) * a(2, 1) - a(0, 1) * a(2, 0)) * d],
        [(a(0, 1) * a(1, 2) - a(0, 2) * a(1, 1)) * d, -(a(0, 0) * a(1, 2) - a(0, 2) * a(1, 0)) * d, (a(0, 0) * a(1, 1) - a(0, 1) * a(1, 0)) * d],
    ]
}
fn apply_normal(n: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    unit([0, 1, 2].map(|r| n[r][0] * v[0] + n[r][1] * v[1] + n[r][2] * v[2]))
}
fn translate(t: [f32; 3]) -> M4 {
    let mut m = I4;
    m[3] = [t[0], t[1], t[2], 1.0];
    m
}
fn scale(s: [f32; 3]) -> M4 {
    [[s[0], 0.0, 0.0, 0.0], [0.0, s[1], 0.0, 0.0], [0.0, 0.0, s[2], 0.0], [0.0, 0.0, 0.0, 1.0]]
}
/// A turn about axis 0 (X), 1 (Y) or 2 (Z), degrees.
fn rot(axis: usize, deg: f32) -> M4 {
    let (s, c) = deg.to_radians().sin_cos();
    let mut m = I4;
    let (i, j) = [(1, 2), (2, 0), (0, 1)][axis];
    m[i][i] = c;
    m[j][j] = c;
    m[i][j] = s;
    m[j][i] = -s;
    m
}
fn quat(q: [f32; 4]) -> M4 {
    // (x, y, z, w)
    let [x, y, z, w] = q;
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + z * w), 2.0 * (x * z - y * w), 0.0],
        [2.0 * (x * y - z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + x * w), 0.0],
        [2.0 * (x * z + y * w), 2.0 * (y * z - x * w), 1.0 - 2.0 * (x * x + y * y), 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// Adds a mesh's triangles under transform `m`.
fn add_mesh(out: &mut Model, m: &M4, positions: &[[f32; 3]], normals: Option<&[[f32; 3]]>, colors: &[[f32; 4]], extra: &[VExtra], indices: &[u32]) {
    let base = out.positions.len() as u32;
    out.extra.extend((0..positions.len()).map(|i| extra.get(i).or(extra.last()).copied().unwrap_or(VExtra::NONE)));
    let nm = normal_matrix(m);
    out.positions.extend(positions.iter().map(|&p| apply(m, p)));
    // Normals only if every mesh has them (otherwise all are worked out).
    match normals {
        Some(n) if n.len() == positions.len() && out.normals.len() == base as usize => out.normals.extend(n.iter().map(|&v| apply_normal(&nm, v))),
        _ => out.normals.clear(),
    }
    out.colors.extend((0..positions.len()).map(|i| colors.get(i).copied().unwrap_or(DEFAULT_COLOR)));
    // A mirroring transform turns faces inside out: keep them facing out.
    let det = dot(cross([m[0][0], m[0][1], m[0][2]], [m[1][0], m[1][1], m[1][2]]), [m[2][0], m[2][1], m[2][2]]);
    for t in indices.chunks_exact(3) {
        if det < 0.0 {
            out.indices.extend([base + t[0], base + t[2], base + t[1]]);
        } else {
            out.indices.extend(t.iter().map(|i| base + i));
        }
    }
}

/// Fan-triangulate polygons given as vertex counts and indices.
fn fans(counts: impl Iterator<Item = usize>, idx: &[u32]) -> (Vec<u32>, Vec<usize>) {
    let (mut out, mut face_of) = (Vec::new(), Vec::new());
    let mut at = 0;
    for (f, n) in counts.enumerate() {
        if at + n > idx.len() {
            break;
        }
        for k in 1..n.saturating_sub(1) {
            out.extend([idx[at], idx[at + k], idx[at + k + 1]]);
            face_of.push(f);
        }
        at += n;
    }
    (out, face_of)
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

// ---- OBJ ----

fn load_obj(path: &Path) -> Result<Model, String> {
    let opts = tobj::LoadOptions { triangulate: true, single_index: true, ..Default::default() };
    let (models, materials) = tobj::load_obj(path, &opts).map_err(|e| format!("not an OBJ file Reelforge can read ({e})"))?;
    let materials = materials.unwrap_or_default();
    let mut out = Model::default();
    let mut all_normals = true;
    // Diffuse textures (map_Kd), each read once, beside the file.
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut pics: Vec<Picture> = Vec::new();
    let mut tex_of: std::collections::HashMap<String, i16> = Default::default();
    for m in &models {
        let mesh = &m.mesh;
        let n = mesh.positions.len() / 3;
        let pos: Vec<[f32; 3]> = (0..n).map(|i| [mesh.positions[i * 3] as f32, mesh.positions[i * 3 + 1] as f32, mesh.positions[i * 3 + 2] as f32]).collect();
        let normals: Option<Vec<[f32; 3]>> = (mesh.normals.len() == n * 3).then(|| (0..n).map(|i| [mesh.normals[i * 3] as f32, mesh.normals[i * 3 + 1] as f32, mesh.normals[i * 3 + 2] as f32]).collect());
        all_normals &= normals.is_some();
        let material = mesh.material_id.and_then(|i| materials.get(i));
        let mat = material.and_then(|m| m.diffuse).map(|d| [d[0] as f32, d[1] as f32, d[2] as f32, 1.0]);
        let tex = material.and_then(|m| m.diffuse_texture.as_ref()).filter(|t| !t.trim().is_empty()).map_or(-1, |t| {
            *tex_of.entry(t.clone()).or_insert_with(|| {
                // The last word is the file (options like -bm 1 come first); backslashes from Windows.
                let file = t.split_whitespace().last().unwrap_or(t).replace('\\', "/");
                match read_picture(&dir.join(&file)) {
                    Some(p) => {
                        pics.push(p);
                        (pics.len() - 1) as i16
                    }
                    None => -1,
                }
            })
        });
        let part = part_index(&mut out.parts, material.map_or(m.name.as_str(), |mm| mm.name.as_str()));
        let uvs = mesh.texcoords.len() == n * 2;
        let extra: Vec<VExtra> = (0..n).map(|i| VExtra { uv: if uvs { [mesh.texcoords[i * 2] as f32, 1.0 - mesh.texcoords[i * 2 + 1] as f32] } else { [0.0; 2] }, tex: if uvs { tex } else { -1 }, part }).collect();
        // A textured material's colour is the texture's (times Kd, usually white).
        let colors: Vec<[f32; 4]> = if mesh.vertex_color.len() == n * 3 {
            (0..n).map(|i| [mesh.vertex_color[i * 3] as f32, mesh.vertex_color[i * 3 + 1] as f32, mesh.vertex_color[i * 3 + 2] as f32, 1.0]).collect()
        } else {
            vec![mat.unwrap_or(DEFAULT_COLOR); n]
        };
        add_mesh(&mut out, &I4, &pos, normals.as_deref(), &colors, &extra, &mesh.indices);
    }
    if !all_normals {
        out.normals.clear();
    }
    out.atlas = pack_atlas(&pics).map(std::sync::Arc::new);
    Ok(out)
}

// ---- glTF / GLB ----

fn load_gltf(path: &Path) -> Result<Model, String> {
    let (doc, buffers, images) = gltf::import(path).map_err(|e| format!("not a glTF file Reelforge can read ({e})"))?;
    // The images, as RGBA8 (the atlas packs them).
    let pics: Vec<Picture> = images.iter().map(to_picture).collect();
    let mut out = Model::default();
    let mut all_normals = true;
    let scene = doc.default_scene().or_else(|| doc.scenes().next()).ok_or("the file has no scene")?;
    let mut stack: Vec<(gltf::Node, M4)> = scene.nodes().map(|n| (n, I4)).collect();
    while let Some((node, parent)) = stack.pop() {
        let m = mul(&parent, &node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            for prim in mesh.primitives() {
                if prim.mode() != gltf::mesh::Mode::Triangles {
                    continue;
                }
                let r = prim.reader(|b| Some(&buffers[b.index()]));
                let Some(pos) = r.read_positions() else { continue };
                let pos: Vec<[f32; 3]> = pos.collect();
                let normals: Option<Vec<[f32; 3]>> = r.read_normals().map(|n| n.collect());
                all_normals &= normals.is_some();
                let material = prim.material();
                let pbr = material.pbr_metallic_roughness();
                let base = pbr.base_color_factor();
                let part = part_index(&mut out.parts, material.name().or(mesh.name()).or(node.name()).unwrap_or("Model"));
                // The base-colour texture, read through its coordinates.
                let tex = pbr.base_color_texture().and_then(|t| {
                    let uv: Vec<[f32; 2]> = r.read_tex_coords(t.tex_coord())?.into_f32().collect();
                    (uv.len() == pos.len()).then_some((t.texture().source().index() as i16, uv))
                });
                let extra: Vec<VExtra> = match &tex {
                    Some((i, uv)) => uv.iter().map(|&uv| VExtra { uv, tex: *i, part }).collect(),
                    None => vec![VExtra { part, ..VExtra::NONE }],
                };
                let colors: Vec<[f32; 4]> = match r.read_colors(0) {
                    Some(c) => c.into_rgba_f32().map(|c| [c[0] * base[0], c[1] * base[1], c[2] * base[2], c[3] * base[3]]).collect(),
                    None => vec![base; pos.len()],
                };
                let idx: Vec<u32> = match r.read_indices() {
                    Some(i) => i.into_u32().collect(),
                    None => (0..pos.len() as u32).collect(),
                };
                add_mesh(&mut out, &m, &pos, normals.as_deref(), &colors, &extra, &idx);
            }
        }
        stack.extend(node.children().map(|c| (c, m)));
    }
    if !all_normals {
        out.normals.clear();
    }
    out.atlas = pack_atlas(&pics).map(std::sync::Arc::new);
    Ok(out)
}

/// A glTF image as RGBA8 (grey and grey-alpha spread to colour; 16-bit and
/// float images are rare in base colours and come out white).
fn to_picture(img: &gltf::image::Data) -> Picture {
    use gltf::image::Format;
    let ch = match img.format {
        Format::R8 => 1,
        Format::R8G8 => 2,
        Format::R8G8B8 => 3,
        Format::R8G8B8A8 => 4,
        _ => return Picture { width: 1, height: 1, rgba: vec![255; 4] },
    };
    let px = (img.width * img.height) as usize;
    let mut rgba = Vec::with_capacity(px * 4);
    for p in img.pixels.chunks_exact(ch).take(px) {
        rgba.extend(match ch {
            1 => [p[0], p[0], p[0], 255],
            2 => [p[0], p[0], p[0], p[1]],
            3 => [p[0], p[1], p[2], 255],
            _ => [p[0], p[1], p[2], p[3]],
        });
    }
    rgba.resize(px * 4, 255);
    Picture { width: img.width.max(1), height: img.height.max(1), rgba }
}

// ---- STL ----

fn load_stl(path: &Path) -> Result<Model, String> {
    let mut f = std::io::BufReader::new(std::fs::File::open(path).map_err(|e| format!("couldn't open it ({e})"))?);
    let mesh = stl_io::read_stl(&mut f).map_err(|e| format!("not an STL file Reelforge can read ({e})"))?;
    let positions = mesh.vertices.iter().map(|v| v.0).collect();
    let indices = mesh.faces.iter().flat_map(|t| t.vertices.map(|i| i as u32)).collect();
    // Normals from the faces (STL's own are often missing or wrong); usually Z up.
    Ok(Model { positions, indices, z_up: true, ..Default::default() })
}

// ---- PLY ----

fn load_ply(path: &Path) -> Result<Model, String> {
    let data = std::fs::read(path).map_err(|e| format!("couldn't open it ({e})"))?;
    parse_ply(&data)
}

#[derive(Clone, Copy, PartialEq)]
enum PlyType {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl PlyType {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "char" | "int8" => Self::I8,
            "uchar" | "uint8" => Self::U8,
            "short" | "int16" => Self::I16,
            "ushort" | "uint16" => Self::U16,
            "int" | "int32" => Self::I32,
            "uint" | "uint32" => Self::U32,
            "float" | "float32" => Self::F32,
            "double" | "float64" => Self::F64,
            _ => return None,
        })
    }
    fn size(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }
}

struct PlyProp {
    name: String,
    ty: PlyType,
    /// A list: the type of its length.
    list: Option<PlyType>,
}

struct PlyElem {
    name: String,
    count: usize,
    props: Vec<PlyProp>,
}

fn parse_ply(data: &[u8]) -> Result<Model, String> {
    let end = data.windows(10).position(|w| w == b"end_header").ok_or("not a PLY file")?;
    let header = std::str::from_utf8(&data[..end]).map_err(|_| "not a PLY file")?;
    let mut body = &data[end + 10..];
    // The rest of the header line.
    while let Some((&c, rest)) = body.split_first() {
        body = rest;
        if c == b'\n' {
            break;
        }
    }
    let mut lines = header.lines();
    if lines.next().map(str::trim) != Some("ply") {
        return Err("not a PLY file".into());
    }
    let mut format = "";
    let mut elems: Vec<PlyElem> = Vec::new();
    for l in lines {
        let w: Vec<&str> = l.split_whitespace().collect();
        match w.as_slice() {
            ["format", f, ..] => format = f,
            ["element", name, count] => elems.push(PlyElem { name: name.to_string(), count: count.parse().map_err(|_| "bad PLY header")?, props: Vec::new() }),
            ["property", "list", lt, t, name] => {
                let e = elems.last_mut().ok_or("bad PLY header")?;
                e.props.push(PlyProp { name: name.to_string(), ty: PlyType::parse(t).ok_or("bad PLY type")?, list: Some(PlyType::parse(lt).ok_or("bad PLY type")?) });
            }
            ["property", t, name] => {
                let e = elems.last_mut().ok_or("bad PLY header")?;
                e.props.push(PlyProp { name: name.to_string(), ty: PlyType::parse(t).ok_or("bad PLY type")?, list: None });
            }
            _ => {}
        }
    }
    let ascii = format == "ascii";
    let big = format == "binary_big_endian";
    let mut words = if ascii { std::str::from_utf8(body).map_err(|_| "bad PLY data")?.split_ascii_whitespace() } else { "".split_ascii_whitespace() };
    let mut at = 0usize;
    let mut read = |ty: PlyType| -> Result<f64, String> {
        if ascii {
            return words.next().and_then(|w| w.parse::<f64>().ok()).ok_or_else(|| "the PLY data ends early".to_string());
        }
        let n = ty.size();
        let b = body.get(at..at + n).ok_or("the PLY data ends early")?;
        at += n;
        let mut a = [0u8; 8];
        a[..n].copy_from_slice(b);
        if big {
            a[..n].reverse();
        }
        Ok(match ty {
            PlyType::I8 => a[0] as i8 as f64,
            PlyType::U8 => a[0] as f64,
            PlyType::I16 => i16::from_le_bytes([a[0], a[1]]) as f64,
            PlyType::U16 => u16::from_le_bytes([a[0], a[1]]) as f64,
            PlyType::I32 => i32::from_le_bytes([a[0], a[1], a[2], a[3]]) as f64,
            PlyType::U32 => u32::from_le_bytes([a[0], a[1], a[2], a[3]]) as f64,
            PlyType::F32 => f32::from_le_bytes([a[0], a[1], a[2], a[3]]) as f64,
            PlyType::F64 => f64::from_le_bytes(a),
        })
    };
    let mut out = Model::default();
    let mut normals = Vec::new();
    for e in &elems {
        let find = |n: &str| e.props.iter().position(|p| p.name == n);
        let (x, y, z) = (find("x"), find("y"), find("z"));
        let (nx, ny, nz) = (find("nx"), find("ny"), find("nz"));
        let (r, g, b, al) = (find("red").or(find("r")), find("green").or(find("g")), find("blue").or(find("b")), find("alpha").or(find("a")));
        let faces = find("vertex_indices").or(find("vertex_index"));
        for _ in 0..e.count {
            let mut vals: Vec<f64> = Vec::with_capacity(e.props.len());
            let mut list: Vec<u32> = Vec::new();
            for (k, p) in e.props.iter().enumerate() {
                match p.list {
                    Some(lt) => {
                        let n = read(lt)? as usize;
                        let items: Vec<f64> = (0..n).map(|_| read(p.ty)).collect::<Result<_, _>>()?;
                        if Some(k) == faces {
                            list = items.iter().map(|v| *v as u32).collect();
                        }
                        vals.push(0.0);
                    }
                    None => vals.push(read(p.ty)?),
                }
            }
            if e.name == "vertex" {
                let get = |i: Option<usize>| i.map(|i| vals[i]);
                out.positions.push([get(x).unwrap_or(0.0) as f32, get(y).unwrap_or(0.0) as f32, get(z).unwrap_or(0.0) as f32]);
                if let (Some(a), Some(b2), Some(c)) = (get(nx), get(ny), get(nz)) {
                    normals.push([a as f32, b2 as f32, c as f32]);
                }
                if let (Some(rv), Some(gv), Some(bv)) = (r, g, b) {
                    // 8-bit colours are 0–255 sRGB; float ones 0–1.
                    let scale = if e.props[rv].ty == PlyType::U8 { 255.0 } else { 1.0 };
                    let c = |v: f64| srgb_to_linear((v / scale) as f32);
                    out.colors.push([c(vals[rv]), c(vals[gv]), c(vals[bv]), al.map_or(1.0, |a| (vals[a] / scale) as f32)]);
                }
            } else if e.name == "face" {
                let (t, _) = fans(std::iter::once(list.len()), &list);
                out.indices.extend(t);
            }
        }
    }
    if normals.len() == out.positions.len() {
        out.normals = normals;
    }
    Ok(out)
}

// ---- FBX (binary 7.x) ----

fn load_fbx(path: &Path) -> Result<Model, String> {
    use fbxcel::low::v7400::AttributeValue as A;
    use fbxcel::tree::any::AnyTree;
    let f = std::io::BufReader::new(std::fs::File::open(path).map_err(|e| format!("couldn't open it ({e})"))?);
    let tree = match AnyTree::from_seekable_reader(f) {
        Ok(AnyTree::V7400(_, tree, _)) => tree,
        Ok(_) => return Err("this FBX version isn't supported (save it as FBX 7.x binary)".into()),
        Err(e) => return Err(format!("not a binary FBX file Reelforge can read ({e}); ASCII FBX needs saving as binary")),
    };
    let root = tree.root();
    let id = |n: &fbxcel::tree::v7400::NodeHandle| match n.attributes().first() {
        Some(A::I64(v)) => Some(*v),
        _ => None,
    };
    let f64s = |n: Option<fbxcel::tree::v7400::NodeHandle>| -> Vec<f64> {
        match n.and_then(|n| n.attributes().first().cloned()) {
            Some(A::ArrF64(v)) => v,
            Some(A::ArrF32(v)) => v.into_iter().map(|x| x as f64).collect(),
            _ => Vec::new(),
        }
    };
    let i32s = |n: Option<fbxcel::tree::v7400::NodeHandle>| -> Vec<i32> {
        match n.and_then(|n| n.attributes().first().cloned()) {
            Some(A::ArrI32(v)) => v,
            Some(A::ArrI64(v)) => v.into_iter().map(|x| x as i32).collect(),
            _ => Vec::new(),
        }
    };
    let num = |a: &A| match a {
        A::F64(v) => *v,
        A::F32(v) => *v as f64,
        A::I32(v) => *v as f64,
        A::I64(v) => *v as f64,
        A::I16(v) => *v as f64,
        _ => 0.0,
    };
    // Properties70 "P" entries: name → numbers.
    let props = |n: &fbxcel::tree::v7400::NodeHandle| -> std::collections::HashMap<String, Vec<f64>> {
        let mut out = std::collections::HashMap::new();
        if let Some(p70) = n.first_child_by_name("Properties70") {
            for p in p70.children_by_name("P") {
                let a = p.attributes();
                if let Some(A::String(name)) = a.first() {
                    out.insert(name.clone(), a.iter().skip(4).map(num).collect());
                }
            }
        }
        out
    };
    let mut up_z = false;
    if let Some(gs) = root.first_child_by_name("GlobalSettings") {
        let p = props(&gs);
        up_z = p.get("UpAxis").and_then(|v| v.first()).is_some_and(|v| *v == 2.0);
    }
    let objects = root.first_child_by_name("Objects").ok_or("the FBX file has no objects")?;
    struct Geo {
        pos: Vec<[f32; 3]>,
        poly: Vec<i32>,
        mats: Vec<i32>,
        by_polygon: bool,
    }
    let mut geos: std::collections::HashMap<i64, Geo> = Default::default();
    let mut models: std::collections::HashMap<i64, M4> = Default::default();
    let mut mat_color: std::collections::HashMap<i64, [f32; 4]> = Default::default();
    let mut mat_name: std::collections::HashMap<i64, String> = Default::default();
    for o in objects.children() {
        let Some(oid) = id(&o) else { continue };
        match o.name() {
            "Geometry" => {
                let v = f64s(o.first_child_by_name("Vertices"));
                let pos = v.chunks_exact(3).map(|c| [c[0] as f32, c[1] as f32, c[2] as f32]).collect();
                let poly = i32s(o.first_child_by_name("PolygonVertexIndex"));
                let lm = o.first_child_by_name("LayerElementMaterial");
                let mats = i32s(lm.and_then(|l| l.first_child_by_name("Materials")));
                let by_polygon = lm.and_then(|l| l.first_child_by_name("MappingInformationType")).and_then(|m| match m.attributes().first() {
                    Some(A::String(s)) => Some(s == "ByPolygon"),
                    _ => None,
                }) == Some(true);
                geos.insert(oid, Geo { pos, poly, mats, by_polygon });
            }
            "Model" => {
                let p = props(&o);
                let v3 = |k: &str, d: f64| p.get(k).filter(|v| v.len() >= 3).map_or([d as f32; 3], |v| [v[0] as f32, v[1] as f32, v[2] as f32]);
                let (t, r, s, pre) = (v3("Lcl Translation", 0.0), v3("Lcl Rotation", 0.0), v3("Lcl Scaling", 1.0), v3("PreRotation", 0.0));
                // Euler XYZ: X first, then Y, then Z.
                let euler = |e: [f32; 3]| mul(&rot(2, e[2]), &mul(&rot(1, e[1]), &rot(0, e[0])));
                let m = mul(&translate(t), &mul(&euler(pre), &mul(&euler(r), &scale(s))));
                models.insert(oid, m);
            }
            "Material" => {
                let p = props(&o);
                let c = p.get("DiffuseColor").or(p.get("Diffuse")).filter(|v| v.len() >= 3).map_or(DEFAULT_COLOR, |v| [v[0] as f32, v[1] as f32, v[2] as f32, 1.0]);
                let k = p.get("DiffuseFactor").and_then(|v| v.first()).map_or(1.0, |v| *v as f32);
                mat_color.insert(oid, [c[0] * k, c[1] * k, c[2] * k, 1.0]);
                // Its name: "Name\0\x01Material".
                if let Some(A::String(n)) = o.attributes().get(1) {
                    mat_name.insert(oid, n.split('\0').next().unwrap_or("").to_string());
                }
            }
            _ => {}
        }
    }
    // Connections: child → parent (object to object).
    let mut parent: std::collections::HashMap<i64, i64> = Default::default();
    let mut geo_model: Vec<(i64, i64)> = Vec::new();
    let mut model_mats: std::collections::HashMap<i64, Vec<i64>> = Default::default();
    if let Some(conns) = root.first_child_by_name("Connections") {
        for c in conns.children_by_name("C") {
            let a = c.attributes();
            let (Some(A::String(kind)), Some(A::I64(child)), Some(A::I64(par))) = (a.first(), a.get(1), a.get(2)) else { continue };
            if kind != "OO" {
                continue;
            }
            if geos.contains_key(child) && models.contains_key(par) {
                geo_model.push((*child, *par));
            } else if models.contains_key(child) {
                parent.insert(*child, *par);
            } else if mat_color.contains_key(child) && models.contains_key(par) {
                model_mats.entry(*par).or_default().push(*child);
            }
        }
    }
    let world = |mut m: i64| -> M4 {
        let mut w = I4;
        let mut guard = 0;
        while let Some(local) = models.get(&m) {
            w = mul(local, &w);
            match parent.get(&m) {
                Some(p) if guard < 64 => {
                    m = *p;
                    guard += 1;
                }
                _ => break,
            }
        }
        w
    };
    let mut out = Model { z_up: up_z, ..Default::default() };
    for (g, m) in geo_model {
        let geo = &geos[&g];
        // Polygons end at a negative index (its bitwise NOT).
        let (mut counts, mut idx) = (Vec::new(), Vec::new());
        let mut n = 0;
        for &v in &geo.poly {
            idx.push(if v < 0 { !v } else { v } as u32);
            n += 1;
            if v < 0 {
                counts.push(n);
                n = 0;
            }
        }
        let (tris, face_of) = fans(counts.into_iter(), &idx);
        let mats: Vec<i64> = model_mats.get(&m).cloned().unwrap_or_default();
        let mat_of = |face: usize| -> Option<i64> {
            let k = if geo.by_polygon { geo.mats.get(face).copied().unwrap_or(0) } else { geo.mats.first().copied().unwrap_or(0) };
            mats.get(k.max(0) as usize).copied()
        };
        // Each triangle its own corners (colours can differ per face).
        let mut pos = Vec::with_capacity(tris.len());
        let mut colors = Vec::with_capacity(tris.len());
        let mut extra = Vec::with_capacity(tris.len());
        for (t, tri) in tris.chunks_exact(3).enumerate() {
            let mat = mat_of(face_of[t]);
            let c = mat.and_then(|k| mat_color.get(&k)).copied().unwrap_or(DEFAULT_COLOR);
            let part = part_index(&mut out.parts, mat.and_then(|k| mat_name.get(&k)).map_or("Model", |s| s.as_str()));
            for &i in tri {
                pos.push(geo.pos.get(i as usize).copied().unwrap_or([0.0; 3]));
                colors.push(c);
                extra.push(VExtra { part, ..VExtra::NONE });
            }
        }
        let seq: Vec<u32> = (0..pos.len() as u32).collect();
        add_mesh(&mut out, &world(m), &pos, None, &colors, &extra, &seq);
    }
    out.normals.clear();
    Ok(weld(out))
}

/// Merge corners at the same place with the same colour (so normals are
/// shared across faces again).
fn weld(m: Model) -> Model {
    let mut map: std::collections::HashMap<([u32; 3], [u32; 4], u16), u32> = Default::default();
    let mut out = Model { z_up: m.z_up, parts: m.parts.clone(), atlas: m.atlas.clone(), ..Default::default() };
    for &i in &m.indices {
        let (p, c) = (m.positions[i as usize], m.colors[i as usize]);
        let x = m.extra.get(i as usize).copied().unwrap_or(VExtra::NONE);
        let k = (p.map(f32::to_bits), c.map(f32::to_bits), x.part);
        let j = *map.entry(k).or_insert_with(|| {
            out.positions.push(p);
            out.colors.push(c);
            out.extra.push(x);
            (out.positions.len() - 1) as u32
        });
        out.indices.push(j);
    }
    out
}

// ---- USD ----

fn load_usd(path: &Path) -> Result<Model, String> {
    use openusd::usd;
    let stage = usd::Stage::open(path.to_str().ok_or("the file name isn't valid text")?).map_err(|e| format!("not a USD file Reelforge can read ({e})"))?;
    let root = stage.prim(openusd::sdf::Path::abs_root()).map_err(|e| e.to_string())?;
    let mut out = Model::default();
    let up: Option<String> = root.get_metadata::<openusd::tf::Token>("upAxis").ok().flatten().map(|t| t.as_str().to_string());
    out.z_up = up.as_deref() == Some("Z");
    let mut stack: Vec<(usd::Prim, M4)> = root.children().map_err(|e| e.to_string())?.into_iter().map(|p| (p, I4)).collect();
    while let Some((prim, parent)) = stack.pop() {
        if !prim.is_active().unwrap_or(true) {
            continue;
        }
        let ty = prim.type_name().ok().flatten().map(|t| t.as_str().to_string()).unwrap_or_default();
        let vis: Option<openusd::tf::Token> = prim.attribute("visibility").get().ok().flatten();
        if vis.is_some_and(|v| v.as_str() == "invisible") {
            continue;
        }
        let m = mul(&parent, &usd_local(&prim));
        if ty == "Mesh" {
            let part = part_index(&mut out.parts, prim.path().name().unwrap_or("Model"));
            let extra = [VExtra { part, ..VExtra::NONE }];
            let points: Vec<openusd::gf::Vec3f> = prim.attribute("points").get().ok().flatten().unwrap_or_default();
            let counts: Vec<i32> = prim.attribute("faceVertexCounts").get().ok().flatten().unwrap_or_default();
            let idx: Vec<i32> = prim.attribute("faceVertexIndices").get().ok().flatten().unwrap_or_default();
            let pos: Vec<[f32; 3]> = points.iter().map(|p| [p.x, p.y, p.z]).collect();
            let idx: Vec<u32> = idx.iter().map(|&i| i.max(0) as u32).collect();
            let (mut tris, face_of) = fans(counts.iter().map(|&c| c.max(0) as usize), &idx);
            // Left-handed meshes are wound the other way.
            let orient: Option<openusd::tf::Token> = prim.attribute("orientation").get().ok().flatten();
            if orient.is_some_and(|o| o.as_str() == "leftHanded") {
                for t in tris.chunks_exact_mut(3) {
                    t.swap(1, 2);
                }
            }
            let dc: Vec<openusd::gf::Vec3f> = prim.attribute("primvars:displayColor").get().ok().flatten().unwrap_or_default();
            let dc: Vec<[f32; 4]> = dc.iter().map(|c| [c.x, c.y, c.z, 1.0]).collect();
            // Display colour: one, per point, or per face.
            if dc.len() > 1 && dc.len() == counts.len() && dc.len() != pos.len() {
                let mut p2 = Vec::new();
                let mut c2 = Vec::new();
                for (t, tri) in tris.chunks_exact(3).enumerate() {
                    for &i in tri {
                        p2.push(pos.get(i as usize).copied().unwrap_or([0.0; 3]));
                        c2.push(dc[face_of[t]]);
                    }
                }
                let seq: Vec<u32> = (0..p2.len() as u32).collect();
                add_mesh(&mut out, &m, &p2, None, &c2, &extra, &seq);
            } else {
                let colors: Vec<[f32; 4]> = if dc.len() == pos.len() { dc } else { vec![dc.first().copied().unwrap_or(DEFAULT_COLOR); pos.len()] };
                add_mesh(&mut out, &m, &pos, None, &colors, &extra, &tris);
            }
        }
        for c in prim.children().map_err(|e| e.to_string())? {
            stack.push((c, m));
        }
    }
    out.normals.clear();
    Ok(weld(out))
}

/// A USD prim's own transform, from its xformOpOrder.
fn usd_local(prim: &openusd::usd::Prim) -> M4 {
    use openusd::gf;
    let order: Vec<openusd::tf::Token> = prim.attribute("xformOpOrder").get().ok().flatten().unwrap_or_default();
    let mut m = I4;
    for op in order.iter().map(|t| t.as_str().to_string()) {
        if op == "!resetXformStack!" {
            m = I4;
            continue;
        }
        let (inverse, name) = match op.strip_prefix("!invert!") {
            Some(n) => (true, n.to_string()),
            None => (false, op.clone()),
        };
        let a = prim.attribute(name.as_str());
        let v3 = || -> Option<[f32; 3]> {
            a.get::<gf::Vec3d>().ok().flatten().map(|v| [v.x as f32, v.y as f32, v.z as f32]).or_else(|| a.get::<gf::Vec3f>().ok().flatten().map(|v| [v.x, v.y, v.z]))
        };
        let f1 = || -> Option<f32> { a.get::<f32>().ok().flatten().or_else(|| a.get::<f64>().ok().flatten().map(|v| v as f32)) };
        let kind = name.trim_start_matches("xformOp:").split(':').next().unwrap_or("").to_string();
        let local = match kind.as_str() {
            "translate" => v3().map(translate),
            "scale" => v3().map(scale),
            "rotateX" => f1().map(|d| rot(0, d)),
            "rotateY" => f1().map(|d| rot(1, d)),
            "rotateZ" => f1().map(|d| rot(2, d)),
            // rotateXYZ: X first, then Y, then Z (and so on for the other orders).
            k if k.starts_with("rotate") && k.len() == 9 => v3().map(|e| {
                let axes: Vec<usize> = k[6..].chars().map(|c| (c as u8 - b'X') as usize).collect();
                let angle = |ax: usize| e[ax];
                axes.iter().fold(I4, |acc, &ax| mul(&rot(ax, angle(ax)), &acc))
            }),
            "orient" => a
                .get::<gf::Quatf>()
                .ok()
                .flatten()
                .map(|q| quat([q.x, q.y, q.z, q.w]))
                .or_else(|| a.get::<gf::Quatd>().ok().flatten().map(|q| quat([q.x as f32, q.y as f32, q.z as f32, q.w as f32]))),
            "transform" => a.get::<gf::Matrix4d>().ok().flatten().map(|t| {
                // USD rows are basis vectors (row vector · M): column-major here is the same numbers.
                let v = t.0;
                [[v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32], [v[4] as f32, v[5] as f32, v[6] as f32, v[7] as f32], [v[8] as f32, v[9] as f32, v[10] as f32, v[11] as f32], [v[12] as f32, v[13] as f32, v[14] as f32, v[15] as f32]]
            }),
            _ => None,
        };
        let Some(mut local) = local else { continue };
        if inverse {
            local = invert(&local);
        }
        // Ops apply right to left: the first listed is outermost.
        m = mul(&m, &local);
    }
    m
}

/// The inverse of an affine transform.
fn invert(m: &M4) -> M4 {
    let n = normal_matrix(m); // inverse transpose of the 3×3
    let mut r = I4;
    for c in 0..3 {
        for row in 0..3 {
            r[c][row] = n[row][c];
        }
    }
    let t = apply(&r, [m[3][0], m[3][1], m[3][2]]);
    r[3] = [-t[0], -t[1], -t[2], 1.0];
    r
}

/// Models read so far (by file, its change time and the reload count), and why one couldn't be.
type Loaded = Result<std::sync::Arc<Model>, String>;
fn cache() -> &'static std::sync::Mutex<Vec<((std::path::PathBuf, Option<std::time::SystemTime>, u32), Loaded)>> {
    static C: std::sync::OnceLock<std::sync::Mutex<Vec<((std::path::PathBuf, Option<std::time::SystemTime>, u32), Loaded)>>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// The model `src` names (read once, and again when the file changes or
/// it's reloaded).
pub fn cached(src: &reelforge_core::ModelSource) -> Loaded {
    let modified = std::fs::metadata(&src.path).and_then(|m| m.modified()).ok();
    let key = (src.path.clone(), modified, src.rev);
    if let Some((_, v)) = cache().lock().unwrap().iter().find(|(k, _)| *k == key) {
        return v.clone();
    }
    let v = if src.path.exists() { load(&src.path).map(std::sync::Arc::new) } else { Err("the file isn't there any more (moved or renamed?)".into()) };
    let mut c = cache().lock().unwrap();
    c.retain(|(k, _)| k.0 != key.0);
    if c.len() >= 8 {
        let _ = c.remove(0);
    }
    c.push((key, v.clone()));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("rf_models_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn extent(m: &Model) -> ([f32; 3], [f32; 3]) {
        m.positions.iter().fold(([f32::MAX; 3], [f32::MIN; 3]), |(l, h), p| ([0, 1, 2].map(|k| l[k].min(p[k])), [0, 1, 2].map(|k| h[k].max(p[k]))))
    }

    fn unit_normals(m: &Model) {
        assert_eq!(m.normals.len(), m.positions.len());
        assert!(m.normals.iter().all(|n| (dot(*n, *n) - 1.0).abs() < 1e-3));
        assert_eq!(m.colors.len(), m.positions.len());
        assert!(m.indices.len() % 3 == 0 && m.indices.iter().all(|&i| (i as usize) < m.positions.len()));
    }

    const CUBE_OBJ: &str = "mtllib cube.mtl\nusemtl red\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nv 0 0 1\nv 1 0 1\nv 1 1 1\nv 0 1 1\n\
        f 1 4 3 2\nf 5 6 7 8\nf 1 2 6 5\nf 2 3 7 6\nf 3 4 8 7\nf 4 1 5 8\n";

    /// OBJ with its MTL colour; a cube's normals from its faces stay sharp at the edges.
    #[test]
    fn reads_obj_with_materials() {
        let d = dir();
        std::fs::write(d.join("cube.obj"), CUBE_OBJ).unwrap();
        std::fs::write(d.join("cube.mtl"), "newmtl red\nKd 0.8 0.1 0.1\n").unwrap();
        let m = load(&d.join("cube.obj")).unwrap();
        unit_normals(&m);
        assert_eq!(m.indices.len(), 6 * 2 * 3);
        assert!(m.colors.iter().all(|c| (c[0] - 0.8).abs() < 1e-4 && (c[1] - 0.1).abs() < 1e-4));
        // Each normal along an axis (no smoothing across the cube's 90° edges).
        assert!(m.normals.iter().all(|n| n.iter().filter(|v| v.abs() > 0.999).count() == 1), "{:?}", m.normals);
        assert_eq!(extent(&m), ([0.0; 3], [1.0; 3]));
        assert!(!m.z_up);
    }

    /// STL, ASCII and binary: Z up, normals worked out.
    #[test]
    fn reads_stl() {
        let d = dir();
        let tri = "solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 2 0 0\nvertex 0 3 0\nendloop\nendfacet\n\
            facet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 0 3 0\nvertex 0 0 4\nendloop\nendfacet\nendsolid t\n";
        std::fs::write(d.join("t.stl"), tri).unwrap();
        let m = load(&d.join("t.stl")).unwrap();
        unit_normals(&m);
        assert!(m.z_up);
        assert_eq!(m.indices.len(), 6);
        assert_eq!(extent(&m).1, [2.0, 3.0, 4.0]);
        // The same as binary.
        let mut bin = vec![0u8; 80];
        bin.extend(1u32.to_le_bytes());
        for v in [[0.0f32, 0.0, 1.0], [0.0, 0.0, 0.0], [5.0, 0.0, 0.0], [0.0, 5.0, 0.0]] {
            for x in v {
                bin.extend(x.to_le_bytes());
            }
        }
        bin.extend([0u8, 0]);
        std::fs::write(d.join("b.stl"), bin).unwrap();
        let b = load(&d.join("b.stl")).unwrap();
        assert_eq!((b.indices.len(), extent(&b).1), (3, [5.0, 5.0, 0.0]));
    }

    /// PLY, ASCII and binary, with 8-bit vertex colours and a quad face.
    #[test]
    fn reads_ply() {
        let d = dir();
        let ascii = "ply\nformat ascii 1.0\nelement vertex 4\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\n\
            element face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0 255 0 0\n1 0 0 255 0 0\n1 1 0 255 0 0\n0 1 0 255 0 0\n4 0 1 2 3\n";
        std::fs::write(d.join("a.ply"), ascii).unwrap();
        let m = load(&d.join("a.ply")).unwrap();
        unit_normals(&m);
        assert_eq!(m.indices.len(), 6);
        assert!(m.colors.iter().all(|c| c[0] > 0.99 && c[1] < 0.01));
        let mut bin = b"ply\nformat binary_little_endian 1.0\nelement vertex 3\nproperty double x\nproperty double y\nproperty double z\nelement face 1\nproperty list uchar uint vertex_indices\nend_header\n".to_vec();
        for v in [[0.0f64, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 2.0, 0.0]] {
            for x in v {
                bin.extend(x.to_le_bytes());
            }
        }
        bin.push(3);
        for i in [0u32, 1, 2] {
            bin.extend(i.to_le_bytes());
        }
        std::fs::write(d.join("b.ply"), bin).unwrap();
        let b = load(&d.join("b.ply")).unwrap();
        assert_eq!((b.indices.len(), extent(&b).1), (3, [2.0, 2.0, 0.0]));
    }

    /// glTF with an embedded buffer: the node's translation and the material's base colour.
    #[test]
    fn reads_gltf() {
        let d = dir();
        let mut buf = Vec::new();
        for v in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for x in v {
                buf.extend(x.to_le_bytes());
            }
        }
        for i in [0u16, 1, 2] {
            buf.extend(i.to_le_bytes());
        }
        buf.extend([0u8, 0]);
        let b64 = |b: &[u8]| {
            const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut s = String::new();
            for c in b.chunks(3) {
                let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
                for k in 0..4 {
                    s.push(if k <= c.len() { T[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
                }
            }
            s
        };
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0,"translation":[5,0,0]}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1,"material":0}}]}}],
            "materials":[{{"pbrMetallicRoughness":{{"baseColorFactor":[0,0.5,1,1]}}}}],
            "buffers":[{{"byteLength":{},"uri":"data:application/octet-stream;base64,{}"}}],
            "bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":36}},{{"buffer":0,"byteOffset":36,"byteLength":6}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}},{{"bufferView":1,"componentType":5123,"count":3,"type":"SCALAR"}}]}}"#,
            buf.len(),
            b64(&buf)
        );
        std::fs::write(d.join("t.gltf"), json).unwrap();
        let m = load(&d.join("t.gltf")).unwrap();
        unit_normals(&m);
        assert_eq!(extent(&m), ([5.0, 0.0, 0.0], [6.0, 1.0, 0.0]));
        assert!(m.colors.iter().all(|c| *c == [0.0, 0.5, 1.0, 1.0]));
    }

    /// USDA: a mesh under a moved Xform, Z up, with a display colour.
    #[test]
    fn reads_usd() {
        let d = dir();
        let usda = r#"#usda 1.0
(
    upAxis = "Z"
)

def Xform "World"
{
    double3 xformOp:translate = (10, 0, 0)
    uniform token[] xformOpOrder = ["xformOp:translate"]

    def Mesh "Quad"
    {
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
        color3f[] primvars:displayColor = [(1, 0, 0)]
    }
}
"#;
        std::fs::write(d.join("q.usda"), usda).unwrap();
        let m = load(&d.join("q.usda")).unwrap();
        unit_normals(&m);
        assert!(m.z_up);
        assert_eq!(m.indices.len(), 6);
        assert_eq!(extent(&m), ([10.0, 0.0, 0.0], [11.0, 1.0, 0.0]));
        assert!(m.colors.iter().all(|c| *c == [1.0, 0.0, 0.0, 1.0]));
    }

    /// Binary FBX: a geometry under a model moved and scaled, with a material colour.
    #[test]
    fn reads_fbx() {
        use fbxcel::low::FbxVersion;
        use fbxcel::writer::v7400::binary::{FbxFooter, Writer};
        let mut w = Writer::new(std::io::Cursor::new(Vec::new()), FbxVersion::V7_4).unwrap();
        let p = |w: &mut Writer<_>, name: &str, vals: &[f64]| {
            let mut a = w.new_node("P").unwrap();
            a.append_string_direct(name).unwrap();
            for t in ["Vector3D", "", "A"] {
                a.append_string_direct(t).unwrap();
            }
            for v in vals {
                a.append_f64(*v).unwrap();
            }
            drop(a);
            w.close_node().unwrap();
        };
        w.new_node("Objects").unwrap();
        {
            let mut a = w.new_node("Geometry").unwrap();
            a.append_i64(1).unwrap();
            a.append_string_direct("Geometry::q").unwrap();
            a.append_string_direct("Mesh").unwrap();
        }
        w.new_node("Vertices").unwrap().append_arr_f64_from_iter(None, [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0]).unwrap();
        w.close_node().unwrap();
        w.new_node("PolygonVertexIndex").unwrap().append_arr_i32_from_iter(None, [0, 1, 2, !3]).unwrap();
        w.close_node().unwrap();
        w.close_node().unwrap(); // Geometry
        {
            let mut a = w.new_node("Model").unwrap();
            a.append_i64(2).unwrap();
            a.append_string_direct("Model::q").unwrap();
            a.append_string_direct("Mesh").unwrap();
        }
        w.new_node("Properties70").unwrap();
        p(&mut w, "Lcl Translation", &[0.0, 0.0, 5.0]);
        p(&mut w, "Lcl Scaling", &[2.0, 2.0, 2.0]);
        w.close_node().unwrap();
        w.close_node().unwrap(); // Model
        {
            let mut a = w.new_node("Material").unwrap();
            a.append_i64(3).unwrap();
            a.append_string_direct("Material::m").unwrap();
            a.append_string_direct("").unwrap();
        }
        w.new_node("Properties70").unwrap();
        p(&mut w, "DiffuseColor", &[0.0, 1.0, 0.0]);
        w.close_node().unwrap();
        w.close_node().unwrap(); // Material
        w.close_node().unwrap(); // Objects
        w.new_node("Connections").unwrap();
        for (c, par) in [(1i64, 2i64), (2, 0), (3, 2)] {
            let mut a = w.new_node("C").unwrap();
            a.append_string_direct("OO").unwrap();
            a.append_i64(c).unwrap();
            a.append_i64(par).unwrap();
            drop(a);
            w.close_node().unwrap();
        }
        w.close_node().unwrap();
        let bytes = w.finalize_and_flush(&FbxFooter::default()).unwrap().into_inner();
        let d = dir();
        std::fs::write(d.join("q.fbx"), bytes).unwrap();
        let m = load(&d.join("q.fbx")).unwrap();
        unit_normals(&m);
        assert_eq!(m.indices.len(), 6);
        assert_eq!(extent(&m), ([0.0, 0.0, 5.0], [2.0, 2.0, 5.0]));
        assert!(m.colors.iter().all(|c| *c == [0.0, 1.0, 0.0, 1.0]));
        // Not a model file: says so.
        std::fs::write(d.join("x.fbx"), b"; FBX 7.3.0 project file").unwrap();
        assert!(load(&d.join("x.fbx")).unwrap_err().contains("binary"));
        assert!(load(&d.join("x.abc")).is_err());
    }
}
