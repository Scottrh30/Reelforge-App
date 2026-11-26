//! 3MF (3D Manufacturing Format, what 3D printing slicers save): a zip of XML.
//! Its objects' meshes and components (objects made of other objects, in this
//! part or another one — the production extension, as Bambu Studio, Orca and
//! PrusaSlicer save them) are placed by the build's items and transforms, and
//! coloured by their base materials or colour groups.

use super::{add_mesh, mul, part_index, srgb_to_linear, weld, Model, VExtra, DEFAULT_COLOR, I4, M4};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// The most one part of the file is unpacked to.
const MAX_PART: u64 = 1 << 30;
/// How deep components inside components are followed.
const MAX_DEPTH: usize = 16;
/// The most triangles read, all told.
const MAX_TRIANGLES: usize = 30_000_000;

pub(super) fn load_3mf(path: &Path) -> Result<Model, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("couldn't open it ({e})"))?;
    read_3mf(std::io::BufReader::new(f))
}

pub(super) fn read_3mf<R: Read + std::io::Seek>(r: R) -> Result<Model, String> {
    let mut zip = zip::ZipArchive::new(r).map_err(|e| format!("not a 3MF file Reelforge can read ({e})"))?;
    let mut parts = Parts { zip: &mut zip, cache: HashMap::new() };
    let root = parts.root_part();
    let mut out = Model { z_up: true, ..Default::default() };
    let mut st = Build { tris: 0 };
    let text = parts.text(&root)?;
    let doc = roxmltree::Document::parse(&text).map_err(|e| format!("its model isn't readable XML ({e})"))?;
    let model = doc.root_element();
    let build = model.children().find(|n| n.has_tag_name_local("build")).ok_or("the file has nothing to print (no build)")?;
    let mut items = 0;
    for item in build.children().filter(|n| n.has_tag_name_local("item")) {
        let Some(id) = item.attribute("objectid") else { continue };
        let part = attr_local(item, "path").map(normalise).unwrap_or_else(|| root.clone());
        let m = item.attribute("transform").and_then(transform).unwrap_or(I4);
        if st.object(&mut parts, &mut out, &part, id, &m, None, 0)? {
            items += 1;
        }
    }
    if items == 0 || out.indices.is_empty() {
        return Err("the file has no meshes to show".into());
    }
    out.normals.clear();
    Ok(weld(out))
}

struct Parts<'a, R: Read + std::io::Seek> {
    zip: &'a mut zip::ZipArchive<R>,
    cache: HashMap<String, std::sync::Arc<String>>,
}

impl<R: Read + std::io::Seek> Parts<'_, R> {
    /// The model part the package's relationships name (usually `3D/3dmodel.model`).
    fn root_part(&mut self) -> String {
        if let Ok(rels) = self.text("_rels/.rels") {
            if let Ok(doc) = roxmltree::Document::parse(&rels) {
                let target = doc.descendants().filter(|n| n.has_tag_name_local("Relationship")).find(|n| n.attribute("Type").is_some_and(|t| t.ends_with("/3dmodel"))).and_then(|n| n.attribute("Target"));
                if let Some(t) = target {
                    return normalise(t);
                }
            }
        }
        let names: Vec<String> = self.zip.file_names().map(str::to_string).collect();
        names.into_iter().find(|n| n.to_lowercase().ends_with(".model")).unwrap_or_else(|| "3D/3dmodel.model".into())
    }

    fn text(&mut self, name: &str) -> Result<std::sync::Arc<String>, String> {
        if let Some(t) = self.cache.get(name) {
            return Ok(t.clone());
        }
        // (Zip names are matched without regard to case, as some writers differ.)
        let actual = self.zip.file_names().find(|n| n.eq_ignore_ascii_case(name)).map(str::to_string).ok_or_else(|| format!("it names a part it doesn't have ({name})"))?;
        let f = self.zip.by_name(&actual).map_err(|e| format!("couldn't read {name} ({e})"))?;
        let mut s = String::new();
        f.take(MAX_PART).read_to_string(&mut s).map_err(|e| format!("couldn't read {name} ({e})"))?;
        let s = std::sync::Arc::new(s);
        self.cache.insert(name.to_string(), s.clone());
        Ok(s)
    }
}

struct Build {
    tris: usize,
}

impl Build {
    /// Adds object `id` of model part `part` under `m`; `paint` is the colour and
    /// part name it inherits. Whether it added anything.
    #[allow(clippy::too_many_arguments)]
    fn object<R: Read + std::io::Seek>(&mut self, parts: &mut Parts<R>, out: &mut Model, part: &str, id: &str, m: &M4, paint: Option<(String, [f32; 4])>, depth: usize) -> Result<bool, String> {
        if depth > MAX_DEPTH {
            return Ok(false);
        }
        let text = parts.text(part)?;
        let doc = roxmltree::Document::parse(&text).map_err(|e| format!("{part} isn't readable XML ({e})"))?;
        let Some(resources) = doc.root_element().children().find(|n| n.has_tag_name_local("resources")) else { return Ok(false) };
        let Some(obj) = resources.children().find(|n| n.has_tag_name_local("object") && n.attribute("id") == Some(id)) else { return Ok(false) };
        let palettes = palettes(resources);
        let name = obj.attribute("name").unwrap_or("Model").to_string();
        let own = obj.attribute("pid").and_then(|p| palettes.get(p)).and_then(|pal| pal.get(obj.attribute("pindex").and_then(|i| i.parse::<usize>().ok()).unwrap_or(0)).cloned());
        let inherited = own.or(paint);
        let mut added = false;
        if let Some(mesh) = obj.children().find(|n| n.has_tag_name_local("mesh")) {
            added |= self.mesh(out, mesh, &palettes, &name, inherited.clone(), m);
        }
        if let Some(comps) = obj.children().find(|n| n.has_tag_name_local("components")) {
            for c in comps.children().filter(|n| n.has_tag_name_local("component")) {
                let Some(cid) = c.attribute("objectid") else { continue };
                let cpart = attr_local(c, "path").map(normalise).unwrap_or_else(|| part.to_string());
                let cm = c.attribute("transform").and_then(transform).unwrap_or(I4);
                added |= self.object(parts, out, &cpart, cid, &mul(m, &cm), inherited.clone(), depth + 1)?;
            }
        }
        Ok(added)
    }

    fn mesh(&mut self, out: &mut Model, mesh: roxmltree::Node, palettes: &HashMap<String, Vec<(String, [f32; 4])>>, object: &str, paint: Option<(String, [f32; 4])>, m: &M4) -> bool {
        let num = |n: roxmltree::Node, a: &str| n.attribute(a).and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| v.is_finite());
        let verts: Vec<[f32; 3]> = mesh
            .children()
            .find(|n| n.has_tag_name_local("vertices"))
            .map(|vs| vs.children().filter(|n| n.has_tag_name_local("vertex")).map(|v| [num(v, "x").unwrap_or(0.0), num(v, "y").unwrap_or(0.0), num(v, "z").unwrap_or(0.0)]).collect())
            .unwrap_or_default();
        let Some(tris) = mesh.children().find(|n| n.has_tag_name_local("triangles")) else { return false };
        let (mut pos, mut colors, mut extra) = (Vec::new(), Vec::new(), Vec::new());
        for t in tris.children().filter(|n| n.has_tag_name_local("triangle")) {
            if self.tris >= MAX_TRIANGLES {
                break;
            }
            let idx = |a: &str| t.attribute(a).and_then(|v| v.trim().parse::<usize>().ok());
            let (Some(a), Some(b), Some(c)) = (idx("v1"), idx("v2"), idx("v3")) else { continue };
            let (Some(&pa), Some(&pb), Some(&pc)) = (verts.get(a), verts.get(b), verts.get(c)) else { continue };
            // Its own material or colour (each corner may have one: p1, p2, p3), else the object's.
            let pal = t.attribute("pid").and_then(|p| palettes.get(p));
            let at = |k: &str| pal.and_then(|p| p.get(idx(k)?).cloned());
            let first = at("p1");
            let corner = [first.clone(), at("p2").or(first.clone()), at("p3").or(first)];
            for (p, own) in [pa, pb, pc].into_iter().zip(corner) {
                let (name, colour) = own.or_else(|| paint.clone()).unwrap_or_else(|| (object.to_string(), DEFAULT_COLOR));
                let part = part_index(&mut out.parts, if name.is_empty() { object } else { &name });
                pos.push(p);
                colors.push(colour);
                extra.push(VExtra { part, ..VExtra::NONE });
            }
            self.tris += 1;
        }
        if pos.is_empty() {
            return false;
        }
        let seq: Vec<u32> = (0..pos.len() as u32).collect();
        add_mesh(out, m, &pos, None, &colors, &extra, &seq);
        true
    }
}

/// Base materials and colour groups by id: each entry's name and linear colour.
fn palettes(resources: roxmltree::Node) -> HashMap<String, Vec<(String, [f32; 4])>> {
    let mut v = HashMap::new();
    for g in resources.children() {
        let Some(id) = g.attribute("id") else { continue };
        let list: Vec<(String, [f32; 4])> = if g.has_tag_name_local("basematerials") {
            g.children().filter(|n| n.has_tag_name_local("base")).map(|b| (b.attribute("name").unwrap_or("").to_string(), b.attribute("displaycolor").and_then(hex).unwrap_or(DEFAULT_COLOR))).collect()
        } else if g.has_tag_name_local("colorgroup") {
            g.children().filter(|n| n.has_tag_name_local("color")).enumerate().map(|(i, c)| (format!("Colour {}", i + 1), c.attribute("color").and_then(hex).unwrap_or(DEFAULT_COLOR))).collect()
        } else {
            continue;
        };
        v.insert(id.to_string(), list);
    }
    v
}

/// `#RRGGBB` or `#RRGGBBAA` (sRGB) as linear RGBA.
fn hex(s: &str) -> Option<[f32; 4]> {
    let h = s.trim().strip_prefix('#')?;
    let byte = |i: usize| h.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok());
    let (r, g, b) = (byte(0)?, byte(2)?, byte(4)?);
    let a = if h.len() >= 8 { byte(6)? } else { 255 };
    let l = |c: u8| srgb_to_linear(c as f32 / 255.0);
    Some([l(r), l(g), l(b), (a as f32 / 255.0).max(0.05)])
}

/// A 3MF transform: twelve numbers, the rows of a 4×3 matrix that points (as
/// rows) are multiplied by.
fn transform(s: &str) -> Option<M4> {
    let v: Vec<f32> = s.split_whitespace().map(|x| x.parse::<f32>().ok().filter(|v| v.is_finite())).collect::<Option<_>>()?;
    let &[a, b, c, d, e, f, g, h, i, j, k, l] = v.as_slice() else { return None };
    Some([[a, b, c, 0.0], [d, e, f, 0.0], [g, h, i, 0.0], [j, k, l, 1.0]])
}

/// A part's name as the zip has it (no leading slash).
fn normalise(p: &str) -> String {
    p.trim().trim_start_matches('/').replace('\\', "/")
}

/// An attribute by its local name, whatever namespace (the production extension's `p:path`).
fn attr_local<'a>(n: roxmltree::Node<'a, '_>, local: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == local && a.namespace().is_some()).map(|a| a.value())
}

trait LocalName {
    fn has_tag_name_local(&self, name: &str) -> bool;
}
impl LocalName for roxmltree::Node<'_, '_> {
    fn has_tag_name_local(&self, name: &str) -> bool {
        self.is_element() && self.tag_name().name() == name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn package(files: &[(&str, &str)]) -> std::io::Cursor<Vec<u8>> {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, text) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(text.as_bytes()).unwrap();
        }
        let mut c = z.finish().unwrap();
        c.set_position(0);
        c
    }

    const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>"#;

    /// A tetrahedron in two base materials, placed twice by the build (once moved), and a
    /// part from another file (the production extension), as a component, scaled.
    #[test]
    fn reads_meshes_materials_components_and_other_parts() {
        let root = r##"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02" xmlns:p="http://schemas.microsoft.com/3dmanufacturing/production/2015/06">
 <resources>
  <basematerials id="1"><base name="Orange PLA" displaycolor="#FF8000"/><base name="White PLA" displaycolor="#FFFFFF"/></basematerials>
  <object id="2" type="model" name="Tetra" pid="1" pindex="0">
   <mesh>
    <vertices><vertex x="0" y="0" z="0"/><vertex x="10" y="0" z="0"/><vertex x="0" y="10" z="0"/><vertex x="0" y="0" z="10"/></vertices>
    <triangles><triangle v1="0" v2="2" v3="1"/><triangle v1="0" v2="1" v3="3"/><triangle v1="0" v2="3" v3="2" pid="1" p1="1"/><triangle v1="1" v2="2" v3="3"/><triangle v1="1" v2="2" v3="99"/></triangles>
   </mesh>
  </object>
  <object id="3" type="model" name="Holder">
   <components><component p:path="/3D/Objects/part.model" objectid="7" transform="2 0 0 0 2 0 0 0 2 100 0 0"/></components>
  </object>
 </resources>
 <build>
  <item objectid="2"/>
  <item objectid="2" transform="1 0 0 0 1 0 0 0 1 0 50 0"/>
  <item objectid="3"/>
 </build>
</model>"##;
        let part = r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"><resources>
 <object id="7" type="model" name="Peg"><mesh>
  <vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices>
  <triangles><triangle v1="0" v2="1" v3="2"/></triangles>
 </mesh></object>
</resources><build/></model>"#;
        let m = read_3mf(package(&[("_rels/.rels", RELS), ("3D/3dmodel.model", root), ("3D/Objects/part.model", part)])).unwrap();
        // Two tetrahedra (the bad triangle left out) and the peg.
        assert_eq!(m.indices.len() / 3, 4 + 4 + 1);
        assert!(m.z_up);
        assert_eq!(m.parts, ["Orange PLA", "White PLA", "Peg"]);
        let has = |p: [f32; 3]| m.positions.iter().any(|q| (0..3).all(|k| (q[k] - p[k]).abs() < 1e-4));
        assert!(has([0.0, 60.0, 0.0]), "the second tetrahedron, moved");
        assert!(has([102.0, 0.0, 0.0]) && has([100.0, 2.0, 0.0]), "the peg, scaled and moved");
        let orange = m.colors[m.extra.iter().position(|x| x.part == 0).unwrap()];
        assert!((orange[0] - 1.0).abs() < 1e-4 && orange[1] > 0.1 && orange[1] < 0.3 && orange[2] == 0.0, "{orange:?}");
        let white = m.extra.iter().filter(|x| x.part == 1).count();
        assert!(white >= 3, "one face white");
    }

    #[test]
    fn colour_groups_and_odd_files() {
        let root = r##"<model xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02" xmlns:m="http://schemas.microsoft.com/3dmanufacturing/material/2015/02"><resources>
 <m:colorgroup id="5"><m:color color="#00FF00"/><m:color color="#0000FF80"/></m:colorgroup>
 <object id="1"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices>
 <triangles><triangle v1="0" v2="1" v3="2" pid="5" p1="0" p2="1" p3="1"/></triangles></mesh></object>
 <object id="9"><components><component objectid="9"/></components></object>
</resources><build><item objectid="1"/><item objectid="9"/><item objectid="404"/></build></model>"##;
        // (No relationships: the .model file is found by itself; a component that is itself
        // is followed only so deep.)
        let m = read_3mf(package(&[("3D/3dmodel.model", root)])).unwrap();
        assert_eq!(m.indices.len(), 3);
        assert_eq!(m.parts, ["Colour 1", "Colour 2"]);
        assert!(m.colors.iter().any(|c| c[3] < 0.6));
        assert!(read_3mf(package(&[("3D/3dmodel.model", "<model><resources/><build/></model>")])).is_err());
        assert!(read_3mf(package(&[("3D/3dmodel.model", "not xml <")])).is_err());
        assert!(read_3mf(std::io::Cursor::new(b"PK not really".to_vec())).is_err());
        assert_eq!(transform("1 2 3"), None);
        assert_eq!(hex("#zz0000"), None);
    }
}
