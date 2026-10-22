//! Cameras, lights and points in the Viewer: their position and point of interest
//! shown where the camera sees them, and dragged across its view.

use crate::{ed, Res};
use reelforge_core::effects::{camera as cam, light as li, point as pt};
use reelforge_core::space3d::{Camera, SceneObject};
use reelforge_core::ClipId;
use reelforge_engine::editor::Editor;
use serde::Serialize;
use std::cell::RefCell;

/// A draggable point of the selected camera or light.
#[derive(Serialize)]
pub struct ScenePoint {
    /// "position" or "target".
    pub which: &'static str,
    /// Where it shows, in frame pixels.
    pub at: [f32; 2],
}

/// The parameters holding a point's X, Y and Z.
fn params(what: SceneObject, which: &str) -> [usize; 3] {
    match (what, which) {
        (SceneObject::Camera, "position") => [cam::X, cam::Y, cam::Z],
        (SceneObject::Camera, _) => [cam::TARGET_X, cam::TARGET_Y, cam::TARGET_Z],
        (SceneObject::Light, "position") => [li::X, li::Y, li::Z],
        (SceneObject::Light, _) => [li::TARGET_X, li::TARGET_Y, li::TARGET_Z],
        (SceneObject::Point, _) => [pt::X, pt::Y, pt::Z],
    }
}

/// The selected clip's camera or light points the active camera can see.
pub fn points(e: &Editor) -> Vec<ScenePoint> {
    let Some((_, c)) = e.selection.and_then(|id| e.seq().find_clip(id)) else { return Vec::new() };
    let (Some(what), Some(fx)) = (c.scene, c.effects.first()) else { return Vec::new() };
    let t = e.playback.playhead;
    if !c.contains(t) {
        return Vec::new();
    }
    let src = c.source_time(t);
    let camera = e.project.scene3d_at(e.active, t).camera;
    let parent = e.project.parent_of(e.active, c, t);
    let which: &[&'static str] = match what {
        SceneObject::Point => &["position"],
        SceneObject::Camera => &["target"],
        SceneObject::Light => match fx.value_at(li::KIND, src) as usize {
            0 => &["position"],
            1 => &["position", "target"],
            2 => &["target"],
            _ => &[],
        },
    };
    which
        .iter()
        .filter_map(|w| {
            let mut p = params(what, w).map(|i| fx.value_at(i, src));
            if let Some(n) = parent {
                p = n.apply(p);
            }
            Some(ScenePoint { which: w, at: camera.project(p)? })
        })
        .collect()
}

/// Where `c` and the point it's parented to show at the playhead (frame
/// pixels), for the line between them in the Viewer. 3D layers and points are
/// seen through the camera; a 2D layer and its parent, flat on the frame.
pub fn parent_link(e: &Editor, c: &reelforge_core::Clip) -> Option<[[f32; 2]; 2]> {
    use reelforge_core::space3d::Null;
    let t = e.playback.playhead;
    if c.parent.is_none() || !c.contains(t) || matches!(c.scene, Some(SceneObject::Camera | SceneObject::Light)) {
        return None;
    }
    let s = e.seq();
    let frame = [s.width as f32, s.height as f32];
    let parent = e.project.parent_of(e.active, c, t)?;
    let (child, three_d) = match c.scene {
        Some(SceneObject::Point) => {
            let own = c.effects.first().map_or(Null::IDENTITY, |fx| Null::from_effect(fx, c.source_time(t)));
            (own.within(&parent).position, true)
        }
        _ => {
            let w = e.project.world_transform(e.active, c, t);
            ([w.position[0], w.position[1], w.depth], w.three_d)
        }
    };
    let flat = |p: [f32; 3]| [frame[0] * 0.5 + p[0], frame[1] * 0.5 + p[1]];
    if three_d {
        let camera = e.project.scene3d_at(e.active, t).camera;
        Some([camera.project(child)?, camera.project(parent.position)?])
    } else {
        Some([flat(child), flat(parent.position)])
    }
}

thread_local! {
    /// The drag under way: the point's starting place and the camera then.
    static DRAG: RefCell<Option<([f32; 3], Camera, Option<reelforge_core::space3d::Null>)>> = const { RefCell::new(None) };
}

/// Drag point `which` of camera/light `id` by `d` frame pixels from where it
/// was when the drag started (`start`: true on the first call). Live; finish
/// with `commit`.
#[tauri::command]
pub fn scene_point_drag(id: u64, which: String, d: [f32; 2], start: bool) -> Res<()> {
    ed(|e| {
        let Some((_, c)) = e.seq().find_clip(ClipId(id)) else { return };
        let (Some(what), Some(fx)) = (c.scene, c.effects.first()) else { return };
        let ix = params(what, &which);
        let t = e.playback.playhead;
        let src = c.source_time(t.max(c.timeline_start).min(c.timeline_end()));
        if start {
            let p = ix.map(|i| fx.value_at(i, src));
            let camera = e.project.scene3d_at(e.active, t).camera;
            let parent = e.project.parent_of(e.active, c, t);
            DRAG.with(|s| *s.borrow_mut() = Some((p, camera, parent)));
        }
        let Some((p, camera, parent)) = DRAG.with(|s| *s.borrow()) else { return };
        let (r, down, _) = camera.basis();
        // Moved across the camera's view where it shows, then back into its parent's space.
        let world = parent.map_or(p, |n| n.apply(p));
        let z = camera.view(world)[2].max(reelforge_core::space3d::NEAR);
        let k = z / camera.focal();
        let mut step = [0, 1, 2].map(|a| (r[a] * d[0] + down[a] * d[1]) * k);
        if let Some(n) = parent {
            step = n.inverse().apply_vector(step);
        }
        let moved = [0, 1, 2].map(|a| p[a] + step[a]);
        crate::fx::set_params(e, id, 0, &[(ix[0], moved[0]), (ix[1], moved[1]), (ix[2], moved[2])]);
    })
}

/// Add a camera, or a light of `light` type (0 point, 1 spot, 2 directional, 3 ambient).
#[tauri::command]
pub fn add_scene(camera: bool, light: usize) -> Res<()> {
    ed(|e| e.add_scene_object(if camera { SceneObject::Camera } else { SceneObject::Light }, light))
}

/// The "New" menu: "layer" (a plane of `color`, #rrggbb), "light" (of
/// `light` type), "point", "camera" or "adjustment".
#[tauri::command]
pub fn add_new(what: String, light: usize, color: Option<String>) -> Res<()> {
    ed(|e| match what.as_str() {
        "layer" => e.add_plane(color.as_deref().and_then(parse_hex).unwrap_or([128, 128, 128, 255])),
        "light" => e.add_scene_object(SceneObject::Light, light),
        "point" => e.add_scene_object(SceneObject::Point, 0),
        "camera" => e.add_scene_object(SceneObject::Camera, 0),
        "adjustment" => e.add_adjustment_layer(),
        _ => {}
    })
}

/// #rrggbb → opaque RGBA.
fn parse_hex(s: &str) -> Option<[u8; 4]> {
    let h = s.strip_prefix('#').unwrap_or(s);
    let v = u32::from_str_radix(h, 16).ok().filter(|_| h.len() == 6)?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8, 255])
}

/// Change a plane's colour (#rrggbb).
#[tauri::command]
pub fn set_plane_color(id: u64, color: String) -> Res<()> {
    ed(|e| {
        if let Some(c) = parse_hex(&color) {
            e.set_plane_color(ClipId(id), c);
        }
    })
}

/// Parent a clip to a point clip (`None`: remove its parent).
#[tauri::command]
pub fn set_parent(id: u64, parent: Option<u64>) -> Res<()> {
    ed(|e| e.set_parent(ClipId(id), parent.map(ClipId)))
}
