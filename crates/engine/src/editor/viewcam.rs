//! The Viewer's views of a 3D scene: through the sequence's camera (Active
//! camera), or from the front, the top, the left or anywhere the camera tools
//! take it (Custom), without touching the sequence's own camera. Only the
//! Viewer sees these: exports always use the sequence's camera.

use super::Editor;
use reelforge_core::space3d::{self, Camera};
use serde::Serialize;

/// Which view the Viewer shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ViewMode {
    Active,
    Front,
    Top,
    Left,
    Custom,
}

impl ViewMode {
    pub fn parse(s: &str) -> Option<ViewMode> {
        Some(match s {
            "active" => ViewMode::Active,
            "front" => ViewMode::Front,
            "top" => ViewMode::Top,
            "left" => ViewMode::Left,
            "custom" => ViewMode::Custom,
            _ => return None,
        })
    }
}

/// A view from `mode`'s side, far off with a long lens (so it looks nearly
/// flat), round the middle of the sequence's camera and what it looks at,
/// wide enough to take in both.
pub fn preset(mode: ViewMode, own: &Camera) -> Camera {
    let mid = [0, 1, 2].map(|a| (own.position[a] + own.target[a]) * 0.5);
    let reach = own.frame[0].max(own.frame[1]).max(own.distance()) * 1.6;
    let fov = 20.0f32;
    let d = reach * 0.5 / (fov * 0.5).to_radians().tan();
    let off = match mode {
        // Looking down from above (up is −Y), the scene's depth going up the picture.
        ViewMode::Top => [0.0, -d, -d * 1e-4],
        ViewMode::Left => [-d, 0.0, 0.0],
        _ => [0.0, 0.0, -d],
    };
    Camera { position: [0, 1, 2].map(|a| mid[a] + off[a]), target: mid, roll: 0.0, fov, dof: None, frame: own.frame }
}

impl Editor {
    pub fn view_mode(&self) -> ViewMode {
        self.view.map_or(ViewMode::Active, |v| v.0)
    }

    /// Look from `mode`: Front, Top and Left start afresh round the scene;
    /// Custom carries on from the view now (or the camera's).
    pub fn set_view_mode(&mut self, mode: ViewMode) {
        let own = self.project.own_camera_at(self.active, self.playback.playhead);
        let view = match mode {
            ViewMode::Active => None,
            ViewMode::Custom => Some((mode, self.view.map_or(own, |v| v.1))),
            _ => Some((mode, preset(mode, &own))),
        };
        self.set_view(view);
    }

    /// Set the view outright (`None`: through the sequence's camera).
    pub fn set_view(&mut self, view: Option<(ViewMode, Camera)>) {
        self.view = view;
        self.view_rev += 1;
        self.apply_view();
    }

    /// Point this thread's scene lookups at the view, for the sequence open now.
    pub(crate) fn apply_view(&self) {
        space3d::set_view(self.view.map(|(_, c)| (self.active, c)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::space3d::SceneObject;

    /// Front, Top and Left look at the scene from those sides, only on this
    /// thread and only for the Viewer; Active goes back to the sequence's
    /// camera, and Custom carries on from the view in use.
    #[test]
    fn views_look_from_their_side_and_leave_the_camera_alone() {
        let mut e = Editor::new(None, None);
        e.add_scene_object(SceneObject::Camera, 0);
        let t = e.playback.playhead;
        let own = e.project.scene3d_at(e.active, t).camera;
        e.set_view_mode(ViewMode::Top);
        let top = e.project.scene3d_at(e.active, t).camera;
        assert!(top.basis().2[1] > 0.99, "looking down: {:?}", top.basis().2);
        // The camera, and the point between it and what it looks at, are in view.
        assert!(top.project(own.position).is_some_and(|p| p[0] > 0.0 && p[0] < own.frame[0] && p[1] > 0.0 && p[1] < own.frame[1]));
        assert_eq!(e.project.own_camera_at(e.active, t), own, "the sequence's camera is untouched");
        e.set_view_mode(ViewMode::Left);
        assert!(e.project.scene3d_at(e.active, t).camera.basis().2[0] > 0.99);
        e.set_view_mode(ViewMode::Front);
        assert!(e.project.scene3d_at(e.active, t).camera.basis().2[2] > 0.99);
        let moved = e.view.unwrap().1.orbited(30.0, 0.0);
        e.set_view(Some((ViewMode::Custom, moved)));
        e.set_view_mode(ViewMode::Custom);
        assert_eq!(e.project.scene3d_at(e.active, t).camera.position, moved.position);
        let rev = e.view_rev;
        e.set_view_mode(ViewMode::Active);
        assert!(e.view_rev > rev && e.view.is_none());
        assert_eq!(e.project.scene3d_at(e.active, t).camera, own);
    }
}
