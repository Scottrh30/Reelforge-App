//! The 3D camera tracker in the editor: analyse a footage clip in the
//! background (follow features, then solve the camera; see
//! `reelforge_core::camtrack`), keep the solve on the clip, and make a
//! keyframed camera, points and 3D layers that sit in the shot.

use super::{Editor, TrackJob, TrackMsg, LOCKED};
use crossbeam_channel::unbounded;
use reelforge_core::anim::{Curve, Interp, Key};
use reelforge_core::camtrack::{orientation_facing, CameraTrack, SolveOptions, Track2d};
use reelforge_core::effects::{camera as cam, point as pt};
use reelforge_core::space3d::SceneObject;
use reelforge_core::undo::{AddClip, Batch, Command, SetCameraTrack};
use reelforge_core::*;
use std::sync::Arc;

/// What to make from a solved camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum CamMake {
    /// Only the camera.
    Camera,
    /// An invisible point at the picked features (parent layers to it).
    Point,
    /// A plain solid lying on the picked features' surface.
    Solid,
    /// A title on the surface.
    Title,
    /// 3D text on the surface.
    Text3d,
}

impl Editor {
    /// Analyse clip `id`'s whole shot in the background: follow features
    /// through it, then solve the camera (lens `fov` degrees if known; as a
    /// camera turning in place with `tripod`).
    pub fn start_camera_tracking(&mut self, id: ClipId, fov: Option<f32>, tripod: bool) {
        self.commit_live_edit();
        if self.tracking.is_some() {
            return;
        }
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        if self.seq().is_locked(id) {
            self.status = LOCKED.into();
            return;
        }
        let footage = c.title.is_none() && c.nested.is_none() && c.scene.is_none() && self.project.media(c.media).is_some_and(|m| m.info.video.is_some()) && !self.project.is_still(c.media);
        if !footage {
            self.status = "The 3D camera tracker works on moving footage: select a video clip".into();
            return;
        }
        let Some(media) = self.project.media(c.media).cloned() else { return };
        let frame = self.seq().frame_rate.frame_duration();
        let (a, b) = (c.media_at(c.timeline_start), c.media_at(c.timeline_end() - frame));
        let (from, until) = (a.min(b), a.max(b));
        // Media pixels → frame pixels (the clip's place in the frame, mid-clip).
        let mid = c.timeline_start + Time((c.duration.0 / 2).max(0));
        let map = self.layer_frame_map(c, mid);
        let layer = self.layer_size(c);
        let px = self.frame_px();
        let (tx, rx) = unbounded();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancel.clone();
        self.playback.pause();
        std::thread::Builder::new()
            .name("camera tracker".into())
            .spawn(move || {
                let ptx = tx.clone();
                let r = (|| -> Result<Option<CameraTrack>, String> {
                    let followed = reelforge_media::track_scene(&media.path, &media.info, from, until, &stop, |p| {
                        let _ = ptx.send(TrackMsg::Progress(p * 0.6));
                    })
                    .map_err(|e| format!("{e:#}"))?;
                    let Some((tracks, times)) = followed else { return Ok(None) };
                    let to_frame = |p: [f32; 2]| map.apply([p[0] - layer[0] / 2.0, p[1] - layer[1] / 2.0]);
                    let tracks: Vec<Track2d> = tracks.into_iter().map(|t| Track2d { start: t.start, pos: t.pos.into_iter().map(to_frame).collect() }).collect();
                    let opts = SolveOptions { fov, tripod };
                    let ptx = tx.clone();
                    let sol = reelforge_core::camtrack::solve(&tracks, times.len(), px, opts, &stop, &mut |p| {
                        let _ = ptx.send(TrackMsg::Progress(0.6 + p * 0.4));
                    });
                    match sol {
                        Ok(s) => s.into_track(&tracks, times, px, 0).map(Some).ok_or_else(|| "the camera's motion couldn't be worked out from this shot".into()),
                        Err(reelforge_core::camtrack::SolveError::Cancelled) => Ok(None),
                        Err(e) => Err(e.to_string()),
                    }
                })();
                let _ = tx.send(TrackMsg::CameraDone(r));
            })
            .expect("camera tracker thread");
        self.tracking = Some(TrackJob { seq: self.active, clip: id, rx, cancel, progress: 0.0, mask: None, through: None, camera: true });
        self.status = "Analysing the shot: following details, then solving the camera…".into();
    }

    /// A camera solve finished (`None`: stopped).
    pub(super) fn finish_camera_tracking(&mut self, job: &TrackJob, r: Result<Option<CameraTrack>, String>) {
        match r {
            Err(e) => self.status = format!("3D camera tracking failed: {e}"),
            Ok(None) => self.status = "3D camera tracking stopped".into(),
            Ok(Some(ct)) => {
                let Some(old) = self.project.sequences.get(job.seq).and_then(|s| s.find_clip(job.clip)).map(|(_, c)| c.camera_track.clone()) else { return };
                let (n, pts, err, fov, tripod) = (ct.cameras.len(), ct.points.len(), ct.error, ct.fov, ct.tripod);
                let filled = n.saturating_sub(ct.placed as usize);
                self.execute(Box::new(SetCameraTrack { seq: job.seq, id: job.clip, old, new: Some(Box::new(ct)), label: "Track camera" }));
                let quality = if err < 0.5 {
                    "a very good fit"
                } else if err < 1.0 {
                    "a good fit"
                } else if err < 2.0 {
                    "a fair fit: check the points stay on their details"
                } else {
                    "a poor fit: things may slide (lots of the picture moving on its own?)"
                };
                self.status = format!(
                    "Solved the camera{}: {n} frames, {pts} points, {err:.2} px error ({quality}), lens {fov:.1}°{}. Pick points in the Viewer, then create a camera and layers",
                    if tripod { " (turning in place)" } else { "" },
                    if filled > 0 { format!(", {filled} frames filled in") } else { String::new() },
                );
            }
        }
    }

    /// The solve's frame at the playhead on clip `c`, if inside it.
    pub fn camera_track_frame(&self, c: &Clip) -> Option<usize> {
        let ct = c.camera_track.as_ref()?;
        let t = self.playback.playhead.max(c.timeline_start).min(c.timeline_end() - self.seq().frame_rate.frame_duration());
        ct.frame_at(c.media_at(t)).or(Some(0))
    }

    /// Remove clip `id`'s camera solve.
    pub fn remove_camera_track(&mut self, id: ClipId) {
        self.commit_live_edit();
        let Some(old) = self.seq().find_clip(id).and_then(|(_, c)| c.camera_track.clone()) else { return };
        self.execute(Box::new(SetCameraTrack { seq: self.active, id, old: Some(old), new: None, label: "Remove camera track" }));
        self.status = "Removed the camera track (a camera or layers made from it stay)".into();
    }

    /// Turn clip `id`'s solved scene so the surface through the `picked`
    /// points is the ground, with its middle at the origin.
    pub fn set_camera_ground(&mut self, id: ClipId, picked: &[usize]) {
        self.commit_live_edit();
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let Some(ct) = c.camera_track.as_ref() else { return };
        if picked.len() < 3 {
            self.status = "Pick at least three points on the ground first (Shift+click, or Shift+drag a box)".into();
            return;
        }
        let i = self.camera_track_frame(c).unwrap_or(0);
        let Some(mut g) = ct.with_ground(picked, i) else {
            self.status = "Those points don't make a surface: pick points spread over the ground".into();
            return;
        };
        g.camera = ct.camera;
        let made = ct.camera.is_some_and(|cid| self.seq().find_clip(cid).is_some());
        self.execute(Box::new(SetCameraTrack { seq: self.active, id, old: Some(ct.clone()), new: Some(Box::new(g)), label: "Set ground plane" }));
        self.status = if made {
            "Set the ground plane. The camera made before stays as it was: create the camera again to use the new ground".into()
        } else {
            "Set the ground plane: the picked surface is now the floor, its middle the origin".into()
        };
    }

    /// Make a keyframed camera from clip `id`'s solve (unless one made from
    /// it is still there), and with `what` a point or a 3D layer on the
    /// `picked` points' surface, all as long as the clip. One undo step.
    pub fn create_from_camera_track(&mut self, id: ClipId, what: CamMake, picked: &[usize]) {
        self.commit_live_edit();
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let Some(ct) = c.camera_track.clone() else { return };
        if what != CamMake::Camera && picked.is_empty() {
            self.status = "Pick one or more points in the Viewer first (click; Shift+click or Shift+drag for more)".into();
            return;
        }
        let i = self.camera_track_frame(c).unwrap_or(0);
        let (start, dur) = (c.timeline_start, c.duration);
        let footage = c.clone();
        let rate = self.seq().frame_rate;
        let mut cmds: Vec<Box<dyn Command>> = Vec::new();
        let mut cam_id = ct.camera.filter(|cid| self.seq().find_clip(*cid).is_some());
        let mut new_ct = ct.clone();
        let mut next_track = None;
        let mut new_name = None;
        if cam_id.is_none() || what == CamMake::Camera {
            let (add, track) = self.layer_track(start, dur);
            if let Some(a) = add {
                new_name = Some(a.track.name.clone());
                cmds.push(Box::new(a));
            }
            let nid = ClipId(self.project.alloc_id());
            let mut clip = Clip::new_scene(nid, start, dur, SceneObject::Camera, ct.frame);
            clip.effects[0].params[cam::FOV].value = ct.fov;
            let mut keys: [Vec<Key<f32>>; 7] = Default::default();
            for (k, pose) in ct.cameras.iter().enumerate() {
                let Some(t) = footage.timeline_of_media(ct.times[k]) else { continue };
                if t < start || t >= start + dur {
                    continue;
                }
                let at = clip.source_time(t.snap(rate));
                let vals = [pose.position[0], pose.position[1], pose.position[2], pose.target[0], pose.target[1], pose.target[2], pose.roll];
                for (j, v) in vals.into_iter().enumerate() {
                    if keys[j].last().is_some_and(|l| l.at == at) {
                        continue;
                    }
                    keys[j].push(Key::new(at, v, Interp::Linear));
                }
            }
            for k in &mut keys {
                k.sort_by_key(|x| x.at);
            }
            let fx = &mut clip.effects[0];
            let params = [cam::X, cam::Y, cam::Z, cam::TARGET_X, cam::TARGET_Y, cam::TARGET_Z, cam::ROLL];
            for (j, p) in params.into_iter().enumerate() {
                let ks = std::mem::take(&mut keys[j]);
                if let Some(first) = ks.first() {
                    fx.params[p].value = first.value;
                }
                fx.params[p].keys = Curve(ks);
            }
            // The roll unwound, so it never spins the long way between frames.
            let roll = &mut fx.params[cam::ROLL].keys.0;
            for j in 1..roll.len() {
                let mut v = roll[j].value;
                while v - roll[j - 1].value > 180.0 {
                    v -= 360.0;
                }
                while v - roll[j - 1].value < -180.0 {
                    v += 360.0;
                }
                roll[j].value = v;
            }
            fx.params[cam::FOCUS].value = ct.camera(i).distance();
            cmds.push(Box::new(AddClip { seq: self.active, track, clip }));
            cam_id = Some(nid);
            new_ct.camera = Some(nid);
            next_track = Some(track);
        }
        let mut made = None;
        if what != CamMake::Camera {
            // The layer goes on a new track just above the camera's, or (the
            // camera already made) where a new layer goes.
            let track = match next_track {
                Some(t) => {
                    let s = self.seq();
                    let name = (1..).map(|n| format!("Video {n}")).find(|n| !s.tracks.iter().any(|t| &t.name == n) && new_name.as_ref() != Some(n)).unwrap();
                    cmds.push(Box::new(reelforge_core::undo::AddTrack { seq: self.active, index: t + 1, track: reelforge_core::project::Track::new(name, TrackKind::Video) }));
                    t + 1
                }
                None => {
                    let (add, t) = self.layer_track(start, dur);
                    cmds.extend(add.map(|a| Box::new(a) as Box<dyn Command>));
                    t
                }
            };
            let Some((centre, normal)) = ct.plane(picked, i) else { return };
            let cam = ct.camera(i);
            let depth = cam.view(centre)[2].max(1.0);
            let focal = cam.focal();
            let lid = ClipId(self.project.alloc_id());
            let mut clip = match what {
                CamMake::Point => {
                    let mut p = Clip::new_scene(lid, start, dur, SceneObject::Point, ct.frame);
                    p.effects[0].params[pt::X].value = centre[0];
                    p.effects[0].params[pt::Y].value = centre[1];
                    p.effects[0].params[pt::Z].value = centre[2];
                    p
                }
                CamMake::Solid => Clip::new_plane(lid, start, dur, [235, 235, 235, 255]),
                CamMake::Title | CamMake::Text3d => {
                    let mut spec = TitleSpec::for_frame(self.seq().height);
                    if let Some(fonts) = self.fonts() {
                        spec.font = reelforge_render::text::default_title_font(&fonts);
                    }
                    if what == CamMake::Text3d {
                        spec.text = "3D Text".into();
                        spec.size = (spec.size * 2.0).round();
                        spec.extrude = (spec.size * 0.4).round();
                        spec.shadow = false;
                        spec.side_color = Some([spec.color[0] / 2, spec.color[1] / 2, spec.color[2] / 2, spec.color[3]]);
                    }
                    Clip::new_title(lid, start, dur, spec)
                }
                CamMake::Camera => unreachable!(),
            };
            if what != CamMake::Point {
                let t = &mut clip.transform;
                t.three_d = true;
                t.position = [centre[0], centre[1]];
                t.depth = centre[2];
                t.orientation = orientation_facing(normal);
                // A solid a third of the frame wide where it is; text at its own size.
                let k = match what {
                    CamMake::Solid => ct.frame[0] / 3.0 * depth / (focal * ct.frame[0]),
                    _ => depth / focal,
                };
                t.scale = [k, k];
            }
            cmds.push(Box::new(AddClip { seq: self.active, track, clip }));
            made = Some((lid, track));
        }
        if new_ct != ct {
            cmds.push(Box::new(SetCameraTrack { seq: self.active, id, old: Some(ct.clone()), new: Some(new_ct), label: "Create camera" }));
        }
        if cmds.is_empty() {
            return;
        }
        let name = match what {
            CamMake::Camera => "Create tracked camera",
            CamMake::Point => "Create tracked point",
            CamMake::Solid => "Create tracked solid",
            CamMake::Title => "Create tracked title",
            CamMake::Text3d => "Create tracked 3D text",
        };
        self.execute(Box::new(Batch { name: name.into(), cmds }));
        let made_cam = next_track.is_some();
        if let Some((lid, track)) = made {
            self.follow_new_layer(track);
            self.select_only(Some(lid));
        } else if let (Some(cid), Some(track)) = (cam_id, next_track) {
            self.follow_new_layer(track);
            let _ = cid;
        }
        let cam_note = if made_cam { "with a camera that moves like the real one" } else { "using the tracked camera" };
        self.status = match what {
            CamMake::Camera => "Made a camera that moves like the real one: 3D layers now stay put in the shot".into(),
            CamMake::Point => format!("Made a point at the picked features, {cam_note}: parent layers to it to pin them there"),
            CamMake::Solid => format!("Made a solid on the picked surface, {cam_note}: resize it, or swap in a picture (Replace footage)"),
            CamMake::Title => format!("Made a title on the picked surface, {cam_note}: type its text in the Controls panel"),
            CamMake::Text3d => format!("Made 3D text on the picked surface, {cam_note}: type its text in the Controls panel"),
        };
    }
}
