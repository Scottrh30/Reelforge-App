//! 3D Model Properties: the dialog that comes up when a 3D model is added
//! (and from a model layer later), with a preview to turn round, its parts
//! (show or hide each), their surfaces, and which way is up.

use crate::{ed, Res};
use reelforge_core::ModelSource;
use tauri::ipc::Response;

/// The most triangles the preview draws (larger models show every so many).
const PREVIEW_TRIANGLES: usize = 1_500_000;

/// Pick a model file to set up (nothing chosen: `None`).
#[tauri::command]
pub fn pick_model_file() -> Option<String> {
    rfd::FileDialog::new().add_filter("3D models", &reelforge_core::MODEL_EXTENSIONS).pick_file().map(|p| p.to_string_lossy().into_owned())
}

/// The model `source` names, for the preview, as one binary message: a little-endian
/// u32 length, that much JSON (the parts, counts, and where each array is), padding
/// to 4 bytes, then the arrays (f32s, u16s and u32s) and the textures' PNG.
#[tauri::command]
pub async fn model_preview(source: ModelSource) -> Res<Response> {
    let p = tauri::async_runtime::spawn_blocking(move || std::panic::catch_unwind(|| reelforge_render::model3d::preview(&source, PREVIEW_TRIANGLES)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|_| "the model couldn't be read".to_string())??;
    Ok(Response::new(pack(&p)))
}

fn pack(p: &reelforge_render::model3d::Preview) -> Vec<u8> {
    let mut body: Vec<u8> = Vec::new();
    let mut at = serde_json::Map::new();
    let mut put = |name: &str, bytes: Vec<u8>, body: &mut Vec<u8>| {
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
        at.insert(name.into(), serde_json::json!([body.len(), bytes.len()]));
        body.extend(bytes);
    };
    let f32s = |v: &mut dyn Iterator<Item = f32>| v.flat_map(f32::to_le_bytes).collect::<Vec<u8>>();
    put("positions", f32s(&mut p.positions.iter().flatten().copied()), &mut body);
    put("normals", f32s(&mut p.normals.iter().flatten().copied()), &mut body);
    put("colors", f32s(&mut p.colors.iter().flatten().copied()), &mut body);
    put("uvs", f32s(&mut p.uvs.iter().flatten().copied()), &mut body);
    put("rects", f32s(&mut p.rects.iter().flatten().copied()), &mut body);
    put("parts_of", p.parts_of.iter().flat_map(|v| v.to_le_bytes()).collect(), &mut body);
    put("indices", p.indices.iter().flat_map(|v| v.to_le_bytes()).collect(), &mut body);
    if let Some(png) = &p.atlas_png {
        put("atlas", png.clone(), &mut body);
    }
    let head = serde_json::json!({
        "arrays": at,
        "parts": p.parts.iter().map(|(name, c, tex)| serde_json::json!({ "name": name, "color": c, "textured": tex })).collect::<Vec<_>>(),
        "triangles": p.triangles,
        "z_up": p.z_up,
        "missing_textures": p.missing_textures,
    })
    .to_string()
    .into_bytes();
    let mut out = Vec::with_capacity(head.len() + body.len() + 8);
    out.extend((head.len() as u32).to_le_bytes());
    out.extend(head);
    while !out.len().is_multiple_of(4) {
        out.push(b' ');
    }
    out.extend(body);
    out
}

/// Add the model as set up in the dialog, as a 3D layer at the playhead.
#[tauri::command]
pub fn add_model_layer(source: ModelSource, name: String) -> Res<()> {
    ed(|e| e.add_3d_model_with(source, Some(name)))
}

/// Pick an image for a part's own picture (its diffuse map).
#[tauri::command]
pub fn pick_map_file() -> Option<String> {
    rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "tga", "bmp"]).pick_file().map(|p| p.to_string_lossy().into_owned())
}

/// Pick a folder to look for the model's textures in.
#[tauri::command]
pub fn pick_texture_folder() -> Option<String> {
    rfd::FileDialog::new().pick_folder().map(|p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The preview message: its JSON head says where each array is, and they're all there.
    #[test]
    fn packs_the_preview() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../render/tests/data/scene-5.3.blend");
        let src = ModelSource { materials: vec![reelforge_core::PartLook { part: "Red".into(), emissive: [10, 0, 0], ..Default::default() }], ..ModelSource::new(path) };
        let p = reelforge_render::model3d::preview(&src, PREVIEW_TRIANGLES).unwrap();
        let bytes = pack(&p);
        if let Ok(out) = std::env::var("REELFORGE_PREVIEW_DUMP") {
            std::fs::write(out, &bytes).unwrap();
        }
        let len = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        let head: serde_json::Value = serde_json::from_slice(&bytes[4..4 + len]).unwrap();
        let start = (4 + len + 3) & !3;
        assert_eq!(head["triangles"], 42);
        assert_eq!(head["parts"].as_array().unwrap().len(), 4);
        assert_eq!(head["parts"][0]["name"], "Red");
        assert_eq!(head["parts"][0]["color"], serde_json::json!([255, 0, 0]));
        let [at, n] = [head["arrays"]["indices"][0].as_u64().unwrap() as usize, head["arrays"]["indices"][1].as_u64().unwrap() as usize];
        assert_eq!(n, 42 * 3 * 4);
        assert!(start + at + n <= bytes.len() && (start + at).is_multiple_of(4));
        let verts = head["arrays"]["positions"][1].as_u64().unwrap() as usize / 12;
        assert_eq!(head["arrays"]["parts_of"][1].as_u64().unwrap() as usize, verts * 2);
    }
}
