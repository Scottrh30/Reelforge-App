//! Cameras and lights in the Viewer: their position and point of interest
//! shown where the camera sees them, and dragged across its view.

use crate::{ed, Res};
use reelforge_core::effects::{camera as cam, light as li};
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
    let which: &[&'static str] = match what {
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
            let p = params(what, w).map(|i| fx.value_at(i, src));
            Some(ScenePoint { which: w, at: camera.project(p)? })
        })
        .collect()
}

thread_local! {
    /// The drag under way: the point's starting place and the camera then.
    static DRAG: RefCell<Option<([f32; 3], Camera)>> = const { RefCell::new(None) };
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
            DRAG.with(|s| *s.borrow_mut() = Some((p, camera)));
        }
        let Some((p, camera)) = DRAG.with(|s| *s.borrow()) else { return };
        let (r, down, _) = camera.basis();
        let z = camera.view(p)[2].max(reelforge_core::space3d::NEAR);
        let k = z / camera.focal();
        let moved = [0, 1, 2].map(|a| p[a] + (r[a] * d[0] + down[a] * d[1]) * k);
        crate::fx::set_params(e, id, 0, &[(ix[0], moved[0]), (ix[1], moved[1]), (ix[2], moved[2])]);
    })
}

/// Add a camera, or a light of `light` type (0 point, 1 spot, 2 directional, 3 ambient).
#[tauri::command]
pub fn add_scene(camera: bool, light: usize) -> Res<()> {
    ed(|e| e.add_scene_object(if camera { SceneObject::Camera } else { SceneObject::Light }, light))
}
