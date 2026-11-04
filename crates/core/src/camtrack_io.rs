//! Solved cameras to and from other applications.
//!
//! Out: an After Effects script (`.jsx`: a comp with the camera and a 3D
//! null per point), a Blender script (`.py`: the camera and a point cloud), a
//! Nuke script (`.nk`: a Camera2, with the points as an `.obj` read by a
//! ReadGeo), a `.chan` file (the plain camera path most 3D and tracking
//! applications read), and the whole solve as Reelforge JSON.
//!
//! In: `.chan` (from Nuke, Blender, SynthEyes, 3DEqualizer, PFTrack…), and
//! Reelforge JSON.
//!
//! The editor's world is frame pixels, X right, Y down, Z away from the
//! viewer, the origin at the frame's middle. After Effects is the same but
//! with the origin at the top-left corner. Nuke and `.chan` have Y up and
//! cameras looking down −Z (the editor's world turned half a turn about X),
//! Blender has Z up and Y away; both get the editor's pixels ÷ 100
//! ([`SCALE`]), so a 1920-pixel frame is about 19 units across.

use crate::camtrack::{cam_pose, CamPose, CameraTrack, Pose};
use crate::space3d::Camera;
use crate::time::Time;
use std::fmt::Write as _;

/// Editor pixels → units in Nuke, Blender and `.chan` files.
pub const SCALE: f64 = 0.01;

type V3 = [f64; 3];
type M3 = [[f64; 3]; 3];

/// Where to export to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CamFormat {
    AfterEffects,
    Blender,
    Nuke,
    Chan,
    Json,
}

impl CamFormat {
    pub fn extension(self) -> &'static str {
        match self {
            CamFormat::AfterEffects => "jsx",
            CamFormat::Blender => "py",
            CamFormat::Nuke => "nk",
            CamFormat::Chan => "chan",
            CamFormat::Json => "json",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            CamFormat::AfterEffects => "After Effects script",
            CamFormat::Blender => "Blender script",
            CamFormat::Nuke => "Nuke script",
            CamFormat::Chan => "Camera path (.chan)",
            CamFormat::Json => "Reelforge camera solve",
        }
    }
}

fn v64(v: [f32; 3]) -> V3 {
    [v[0] as f64, v[1] as f64, v[2] as f64]
}

/// The camera's right, down and forward axes and position in frame `i`.
fn axes(ct: &CameraTrack, i: usize) -> (V3, V3, V3, V3) {
    let cam = ct.camera(i);
    let (r, d, f) = cam.basis();
    (v64(r), v64(d), v64(f), v64(cam.position))
}

/// Editor → Nuke/`.chan` (Y up, Z towards the viewer), scaled.
fn to_nuke(v: V3) -> V3 {
    [v[0] * SCALE, -v[1] * SCALE, -v[2] * SCALE]
}
/// Nuke/`.chan` → editor.
fn from_nuke(v: V3) -> V3 {
    [v[0] / SCALE, -v[1] / SCALE, -v[2] / SCALE]
}
/// Editor → Blender (Z up, Y away), scaled.
fn to_blender(v: V3) -> V3 {
    [v[0] * SCALE, v[2] * SCALE, -v[1] * SCALE]
}
/// A direction (not scaled) into Nuke's or Blender's axes.
fn dir_nuke(v: V3) -> V3 {
    [v[0], -v[1], -v[2]]
}
fn dir_blender(v: V3) -> V3 {
    [v[0], v[2], -v[1]]
}
fn neg(v: V3) -> V3 {
    [-v[0], -v[1], -v[2]]
}

/// A camera's rotation matrix in a Y-up, look-down-−Z world (Nuke, Blender):
/// columns right, up, back.
fn yup_matrix(right: V3, up: V3, back: V3) -> M3 {
    [[right[0], up[0], back[0]], [right[1], up[1], back[1]], [right[2], up[2], back[2]]]
}

/// Rotation angles (degrees) about X, Y and Z for Nuke's default order
/// (ZXY: turned about Z, then X, then Y, so R = Ry · Rx · Rz).
pub fn euler_zxy(m: &M3) -> [f64; 3] {
    let rx = (-m[1][2]).clamp(-1.0, 1.0).asin();
    let (ry, rz) = if rx.cos().abs() > 1e-9 {
        (m[0][2].atan2(m[2][2]), m[1][0].atan2(m[1][1]))
    } else {
        // Looking straight up or down: put the turn in Y.
        ((-m[2][0]).atan2(m[0][0]), 0.0)
    };
    [rx.to_degrees(), ry.to_degrees(), rz.to_degrees()]
}

/// The rotation matrix from Nuke's ZXY angles (degrees).
pub fn matrix_zxy(a: [f64; 3]) -> M3 {
    let (x, y, z) = (a[0].to_radians(), a[1].to_radians(), a[2].to_radians());
    let rx = [[1.0, 0.0, 0.0], [0.0, x.cos(), -x.sin()], [0.0, x.sin(), x.cos()]];
    let ry = [[y.cos(), 0.0, y.sin()], [0.0, 1.0, 0.0], [-y.sin(), 0.0, y.cos()]];
    let rz = [[z.cos(), -z.sin(), 0.0], [z.sin(), z.cos(), 0.0], [0.0, 0.0, 1.0]];
    mm(&mm(&ry, &rx), &rz)
}

fn mm(a: &M3, b: &M3) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    o
}

/// Frames per second of the track (from its frame times).
pub fn track_fps(ct: &CameraTrack) -> f64 {
    match ct.times.as_slice() {
        [a, b, ..] if b.0 > a.0 => 1.0 / (*b - *a).as_secs_f64(),
        _ => 25.0,
    }
}

/// The camera in each frame, in Nuke's world: position and ZXY rotation.
fn nuke_frames(ct: &CameraTrack) -> Vec<(V3, [f64; 3])> {
    let mut out: Vec<(V3, [f64; 3])> = Vec::with_capacity(ct.cameras.len());
    for i in 0..ct.cameras.len() {
        let (r, d, f, p) = axes(ct, i);
        let m = yup_matrix(dir_nuke(r), dir_nuke(neg(d)), dir_nuke(neg(f)));
        let mut e = euler_zxy(&m);
        // Keep angles continuous (no jumps of 360° between frames).
        if let Some((_, prev)) = out.last() {
            for k in 0..3 {
                while e[k] - prev[k] > 180.0 {
                    e[k] -= 360.0;
                }
                while e[k] - prev[k] < -180.0 {
                    e[k] += 360.0;
                }
            }
        }
        out.push((to_nuke(p), e));
    }
    out
}

/// The points to export: `picked` (indices into the solve's points) if any, else all.
fn chosen(ct: &CameraTrack, picked: &[usize]) -> Vec<usize> {
    if picked.is_empty() {
        (0..ct.points.len()).collect()
    } else {
        picked.iter().copied().filter(|&k| k < ct.points.len()).collect()
    }
}

fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// An After Effects script: a comp the size of the frame and as long as the
/// track, a camera keyed every frame (position, point of interest, Z
/// rotation, zoom) and a 3D null at each point.
pub fn after_effects(ct: &CameraTrack, name: &str, picked: &[usize]) -> String {
    let [w, h] = ct.frame;
    let fps = track_fps(ct);
    let n = ct.cameras.len();
    let zoom = ct.camera(0).focal();
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let ae = |p: [f32; 3]| format!("[{:.4},{:.4},{:.4}]", p[0] as f64 + cx, p[1] as f64 + cy, p[2]);
    let mut s = String::new();
    let _ = writeln!(s, "// Reelforge camera solve: {name}");
    let _ = writeln!(s, "// File > Scripts > Run Script File… makes a comp with the tracked camera and its points.");
    let _ = writeln!(s, "// Put the footage in the comp as an ordinary (2D) layer, full frame.");
    s.push_str("(function () {\n  app.beginUndoGroup(\"Reelforge camera solve\");\n");
    let _ = writeln!(s, "  var fps = {fps:.6};");
    let _ = writeln!(s, "  var comp = app.project.items.addComp({}, {w}, {h}, 1, Math.max({n} / fps, 1 / fps), fps);", js_string(&format!("{name} (camera)")));
    let times: Vec<String> = (0..n).map(|k| format!("{:.6}", k as f64 / fps)).collect();
    let pos: Vec<String> = ct.cameras.iter().map(|c| ae(c.position)).collect();
    let poi: Vec<String> = ct.cameras.iter().map(|c| ae(c.target)).collect();
    let roll: Vec<String> = ct.cameras.iter().map(|c| format!("{:.4}", c.roll)).collect();
    let _ = writeln!(s, "  var t = [{}];", times.join(","));
    s.push_str("  var cam = comp.layers.addCamera(\"Tracked camera\", [comp.width / 2, comp.height / 2]);\n");
    s.push_str("  cam.autoOrient = AutoOrientType.CAMERA_OR_POINT_OF_INTEREST;\n");
    let _ = writeln!(s, "  cam.property(\"Camera Options\").property(\"Zoom\").setValue({zoom:.4});");
    let _ = writeln!(s, "  cam.property(\"Transform\").property(\"Position\").setValuesAtTimes(t, [{}]);", pos.join(","));
    let _ = writeln!(s, "  cam.property(\"Transform\").property(\"Point of Interest\").setValuesAtTimes(t, [{}]);", poi.join(","));
    let _ = writeln!(s, "  cam.property(\"Transform\").property(\"Z Rotation\").setValuesAtTimes(t, [{}]);", roll.join(","));
    let pts = chosen(ct, picked);
    if !pts.is_empty() {
        let list: Vec<String> = pts.iter().map(|&k| format!("[{},{}]", k + 1, ae(ct.points[k].pos))).collect();
        let _ = writeln!(s, "  var pts = [{}];", list.join(","));
        s.push_str("  for (var i = 0; i < pts.length; i++) {\n    var nl = comp.layers.addNull();\n    nl.threeDLayer = true;\n    nl.name = \"Track point \" + pts[i][0];\n    nl.property(\"Transform\").property(\"Position\").setValue(pts[i][1]);\n    nl.property(\"Transform\").property(\"Scale\").setValue([20, 20, 20]);\n  }\n");
    }
    s.push_str("  comp.openInViewer();\n  app.endUndoGroup();\n})();\n");
    s
}

fn py_v(v: V3) -> String {
    format!("({:.6},{:.6},{:.6})", v[0], v[1], v[2])
}

/// A Blender script: render size and frame rate, a camera keyed every frame
/// and the points as a cloud of vertices (Z up, metres = pixels ÷ 100).
pub fn blender(ct: &CameraTrack, name: &str, picked: &[usize]) -> String {
    let [w, h] = ct.frame;
    let fps = track_fps(ct);
    let mut s = String::new();
    let _ = writeln!(s, "# Reelforge camera solve: {name}");
    let _ = writeln!(s, "# In Blender's Scripting workspace: open this file and press Run Script.");
    let _ = writeln!(s, "# Use the footage as the camera's background image (or a compositor plate).");
    s.push_str("import bpy, math\nfrom mathutils import Matrix\n\n");
    let _ = writeln!(s, "NAME = {}", js_string(name));
    let _ = writeln!(s, "FPS = {fps:.6}");
    let _ = writeln!(s, "FOV = {:.6}  # vertical, degrees", ct.fov);
    s.push_str("# Each frame: position, camera right, up and back axes.\nFRAMES = [\n");
    for i in 0..ct.cameras.len() {
        let (r, d, f, p) = axes(ct, i);
        let _ = writeln!(s, "    ({}, {}, {}, {}),", py_v(to_blender(p)), py_v(dir_blender(r)), py_v(dir_blender(neg(d))), py_v(dir_blender(neg(f))));
    }
    s.push_str("]\nPOINTS = [\n");
    for k in chosen(ct, picked) {
        let _ = writeln!(s, "    {},", py_v(to_blender(v64(ct.points[k].pos))));
    }
    s.push_str("]\n\n");
    let _ = writeln!(s, "scene = bpy.context.scene\nscene.render.resolution_x = {w}\nscene.render.resolution_y = {h}\nscene.render.resolution_percentage = 100");
    s.push_str(
        "scene.render.fps = max(1, round(FPS))\nscene.render.fps_base = scene.render.fps / FPS\nscene.frame_start = 1\nscene.frame_end = len(FRAMES)\n\n\
cam_data = bpy.data.cameras.new(NAME + \" camera\")\ncam_data.sensor_fit = 'VERTICAL'\ncam_data.angle_y = math.radians(FOV)\n\
cam = bpy.data.objects.new(NAME + \" camera\", cam_data)\nscene.collection.objects.link(cam)\nscene.camera = cam\n\
cam.rotation_mode = 'XYZ'\nprev = None\n\
for i, (p, r, u, b) in enumerate(FRAMES):\n\
    m = Matrix(((r[0], u[0], b[0]), (r[1], u[1], b[1]), (r[2], u[2], b[2])))\n\
    e = m.to_euler('XYZ', prev) if prev is not None else m.to_euler('XYZ')\n\
    prev = e\n\
    cam.location = p\n\
    cam.rotation_euler = e\n\
    cam.keyframe_insert(data_path='location', frame=i + 1)\n\
    cam.keyframe_insert(data_path='rotation_euler', frame=i + 1)\n\n\
if POINTS:\n\
    mesh = bpy.data.meshes.new(NAME + \" points\")\n\
    mesh.from_pydata(POINTS, [], [])\n\
    obj = bpy.data.objects.new(NAME + \" points\", mesh)\n\
    scene.collection.objects.link(obj)\n",
    );
    s
}

/// A Nuke script: the frame format and range, a Camera2 keyed every frame,
/// and a ReadGeo of `obj_file` (the points; see [`obj`]).
pub fn nuke(ct: &CameraTrack, name: &str, obj_file: &str) -> String {
    let [w, h] = ct.frame;
    let fps = track_fps(ct);
    let n = ct.cameras.len();
    let frames = nuke_frames(ct);
    let curve = |vals: Vec<f64>| format!("{{curve x1 {}}}", vals.iter().map(|v| format!("{v:.6}")).collect::<Vec<_>>().join(" "));
    // Lens: 36 mm across, the field of view kept.
    let vfov = (ct.fov as f64).to_radians();
    let hap = 36.0;
    let vap = hap * h as f64 / w as f64;
    let focal = vap / 2.0 / (vfov / 2.0).tan();
    let mut s = String::new();
    let _ = writeln!(s, "# Reelforge camera solve: {name}");
    let _ = writeln!(s, "Root {{\n inputs 0\n first_frame 1\n last_frame {n}\n fps {fps:.6}\n format \"{w} {h} 0 0 {w} {h} 1 reelforge_{w}x{h}\"\n}}");
    let _ = writeln!(
        s,
        "Camera2 {{\n inputs 0\n rot_order ZXY\n translate {{{} {} {}}}\n rotate {{{} {} {}}}\n focal {focal:.6}\n haperture {hap:.6}\n vaperture {vap:.6}\n name {}\n label \"Reelforge solve\"\n xpos 0\n ypos 0\n}}",
        curve(frames.iter().map(|f| f.0[0]).collect()),
        curve(frames.iter().map(|f| f.0[1]).collect()),
        curve(frames.iter().map(|f| f.0[2]).collect()),
        curve(frames.iter().map(|f| f.1[0]).collect()),
        curve(frames.iter().map(|f| f.1[1]).collect()),
        curve(frames.iter().map(|f| f.1[2]).collect()),
        nuke_name(name, "Camera"),
    );
    if !ct.points.is_empty() {
        let _ = writeln!(s, "ReadGeo2 {{\n inputs 0\n file {}\n name {}\n xpos 150\n ypos 0\n}}", js_string(obj_file), nuke_name(name, "Points"));
    }
    s
}

fn nuke_name(name: &str, what: &str) -> String {
    let clean: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let clean = clean.trim_matches('_');
    if clean.is_empty() || clean.starts_with(|c: char| c.is_ascii_digit()) {
        format!("Reelforge{what}")
    } else {
        format!("{clean}_{what}")
    }
}

/// The points as an `.obj` (vertices only), in Nuke's units and axes.
pub fn obj(ct: &CameraTrack, picked: &[usize]) -> String {
    let mut s = String::from("# Reelforge camera solve points\n");
    for k in chosen(ct, picked) {
        let p = to_nuke(v64(ct.points[k].pos));
        let _ = writeln!(s, "v {:.6} {:.6} {:.6}", p[0], p[1], p[2]);
    }
    s
}

/// A `.chan` file: a line a frame, from 1: frame, position (x y z), rotation
/// (ZXY degrees) and vertical field of view.
pub fn chan(ct: &CameraTrack) -> String {
    let mut s = String::new();
    for (k, (p, e)) in nuke_frames(ct).into_iter().enumerate() {
        let _ = writeln!(s, "{}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}\t{:.6}", k + 1, p[0], p[1], p[2], e[0], e[1], e[2], ct.fov);
    }
    s
}

/// A camera path read from a `.chan` file: a camera in each frame (the
/// file's frame numbers, from the first) and the field of view (the file's,
/// if it has one, else `fov`).
#[derive(Clone, Debug, PartialEq)]
pub struct ChanPath {
    pub frames: Vec<(i64, CamPose)>,
    pub fov: f32,
}

/// Read a `.chan` file for a frame `frame` pixels.
pub fn read_chan(text: &str, frame: [f32; 2], fov: f32) -> Result<ChanPath, String> {
    let mut frames = Vec::new();
    let mut file_fov = None;
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let vals: Result<Vec<f64>, _> = line.split_whitespace().map(str::parse::<f64>).collect();
        let vals = vals.map_err(|_| format!("line {} isn't numbers: {line}", n + 1))?;
        if vals.len() < 7 {
            return Err(format!("line {} has {} numbers; a .chan line has 7 or 8 (frame, x y z, rotation x y z, field of view)", n + 1, vals.len()));
        }
        if vals.len() >= 8 && vals[7] > 0.0 && vals[7] < 180.0 {
            file_fov = Some(vals[7] as f32);
        }
        let m = matrix_zxy([vals[4], vals[5], vals[6]]);
        // Columns: right, up, back (Nuke) → the editor's right, down, forward.
        let col = |c: usize| [m[0][c], m[1][c], m[2][c]];
        let back_nuke = |v: V3| [v[0], -v[1], -v[2]];
        let (r, d, f) = (back_nuke(col(0)), neg(back_nuke(col(1))), neg(back_nuke(col(2))));
        let pose = Pose { r: [r, d, f], c: from_nuke([vals[1], vals[2], vals[3]]) };
        frames.push((vals[0].round() as i64, pose));
    }
    if frames.is_empty() {
        return Err("there are no camera frames in this file".into());
    }
    frames.sort_by_key(|f| f.0);
    let fov = file_fov.unwrap_or(fov);
    let dist = Camera { position: [0.0; 3], target: [0.0, 0.0, 1.0], roll: 0.0, fov, dof: None, frame }.focal() as f64;
    Ok(ChanPath { frames: frames.into_iter().map(|(k, p)| (k, cam_pose(&p, dist))).collect(), fov })
}

/// A whole solve as JSON.
pub fn json(ct: &CameraTrack) -> String {
    serde_json::to_string_pretty(ct).unwrap_or_default()
}

/// A solve read back from JSON (checked to hang together).
pub fn read_json(text: &str) -> Result<CameraTrack, String> {
    let ct: CameraTrack = serde_json::from_str(text).map_err(|e| format!("this isn't a Reelforge camera solve ({e})"))?;
    if ct.cameras.is_empty() || ct.cameras.len() != ct.times.len() {
        return Err("the camera solve is incomplete (no frames, or frames without times)".into());
    }
    if !(1.0..179.0).contains(&ct.fov) || ct.frame[0] <= 0.0 || ct.frame[1] <= 0.0 {
        return Err("the camera solve's lens or frame size isn't valid".into());
    }
    Ok(CameraTrack { camera: None, ..ct })
}

/// Frame times for a path read from a file: frame `k` (from the first) at `k / fps` after `start`.
pub fn chan_times(path: &ChanPath, start: Time, fps: f64) -> Vec<Time> {
    let first = path.frames.first().map_or(0, |f| f.0);
    path.frames.iter().map(|(k, _)| start + Time::from_secs_f64((k - first) as f64 / fps)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camtrack::ScenePoint;

    fn track() -> CameraTrack {
        let n = 30;
        let cameras = (0..n)
            .map(|k| {
                let s = k as f32 / (n - 1) as f32;
                CamPose { position: [-300.0 + 600.0 * s, -150.0 + 40.0 * s, -1200.0 + 200.0 * s], target: [100.0 * s, 50.0, 600.0], roll: -5.0 + 12.0 * s }
            })
            .collect();
        CameraTrack {
            times: (0..n).map(|k| Time::from_secs_f64(k as f64 / 24.0)).collect(),
            cameras,
            fov: 42.0,
            frame: [1920.0, 1080.0],
            points: vec![
                ScenePoint { pos: [-200.0, 300.0, 400.0], error: 0.2, first: 0, last: 29 },
                ScenePoint { pos: [250.0, -100.0, 900.0], error: 0.3, first: 3, last: 20 },
            ],
            error: 0.25,
            tripod: false,
            placed: 30,
            camera: None,
        }
    }

    #[test]
    fn zxy_angles_round_trip() {
        for a in [[10.0, 20.0, 30.0], [-45.0, 170.0, -60.0], [80.0, -30.0, 5.0], [0.0, 0.0, 0.0]] {
            let m = matrix_zxy(a);
            let b = euler_zxy(&m);
            let m2 = matrix_zxy(b);
            for i in 0..3 {
                for j in 0..3 {
                    assert!((m[i][j] - m2[i][j]).abs() < 1e-9, "{a:?} → {b:?}");
                }
            }
        }
    }

    #[test]
    fn chan_round_trips_the_camera() {
        let ct = track();
        let text = chan(&ct);
        assert_eq!(text.lines().count(), 30);
        assert!(text.starts_with("1\t"));
        let back = read_chan(&text, ct.frame, 30.0).unwrap();
        assert_eq!(back.fov, 42.0, "the file's lens");
        assert_eq!(back.frames.len(), 30);
        for (k, (frame, pose)) in back.frames.iter().enumerate() {
            assert_eq!(*frame, k as i64 + 1);
            let (a, b) = (ct.camera(k), Camera { position: pose.position, target: pose.target, roll: pose.roll, fov: back.fov, dof: None, frame: ct.frame });
            let (ab, bb) = (a.basis(), b.basis());
            for j in 0..3 {
                assert!((a.position[j] - b.position[j]).abs() < 1e-2, "{k}: {:?} {:?}", a.position, b.position);
                assert!((ab.0[j] - bb.0[j]).abs() < 1e-4 && (ab.1[j] - bb.1[j]).abs() < 1e-4 && (ab.2[j] - bb.2[j]).abs() < 1e-4, "{k}: axes");
            }
            // Points project to the same place.
            for p in &ct.points {
                let (pa, pb) = (a.project(p.pos).unwrap(), b.project(p.pos).unwrap());
                assert!((pa[0] - pb[0]).abs() < 0.05 && (pa[1] - pb[1]).abs() < 0.05);
            }
        }
        // Without a lens column the given one is used; bad files are refused.
        let seven: String = text.lines().map(|l| l.split('\t').take(7).collect::<Vec<_>>().join(" ") + "\n").collect();
        assert_eq!(read_chan(&seven, ct.frame, 33.0).unwrap().fov, 33.0);
        assert!(read_chan("1 2 3\n", ct.frame, 30.0).is_err());
        assert!(read_chan("# nothing\n", ct.frame, 30.0).is_err());
        assert!(read_chan("1 a b c d e f\n", ct.frame, 30.0).is_err());
    }

    #[test]
    fn nuke_camera_sees_points_where_the_editor_does() {
        // Rebuild Nuke's camera from the exported numbers and project a point
        // the way Nuke does (camera looks down −Z, Y up, focal over aperture).
        let ct = track();
        let frames = nuke_frames(&ct);
        let [w, h] = ct.frame;
        let vfov = (ct.fov as f64).to_radians();
        let half_h = (vfov / 2.0).tan();
        for (k, (p, e)) in frames.iter().enumerate() {
            let m = matrix_zxy(*e);
            for pt in &ct.points {
                let x = to_nuke(v64(pt.pos));
                let rel = [x[0] - p[0], x[1] - p[1], x[2] - p[2]];
                // Into camera space: Rᵀ · rel.
                let c = [(0..3).map(|i| m[i][0] * rel[i]).sum::<f64>(), (0..3).map(|i| m[i][1] * rel[i]).sum::<f64>(), (0..3).map(|i| m[i][2] * rel[i]).sum::<f64>()];
                assert!(c[2] < 0.0, "in front: looking down −Z");
                let (nx, ny) = (c[0] / -c[2], c[1] / -c[2]);
                let u = w as f64 / 2.0 + nx / half_h * h as f64 / 2.0;
                let v = h as f64 / 2.0 - ny / half_h * h as f64 / 2.0;
                let want = ct.project(k, pt.pos).unwrap();
                assert!((u - want[0] as f64).abs() < 0.05 && (v - want[1] as f64).abs() < 0.05, "{k}: {u},{v} vs {want:?}");
            }
        }
        let nk = nuke(&ct, "My shot", "/tmp/points.obj");
        assert!(nk.contains("Camera2 {") && nk.contains("rot_order ZXY") && nk.contains("translate {{curve x1 ") && nk.contains("name My_shot_Camera"));
        assert!(nk.contains("ReadGeo2 {") && nk.contains("\"/tmp/points.obj\""));
        assert!(nk.contains("last_frame 30") && nk.contains("format \"1920 1080 0 0 1920 1080 1"));
        // Lens: 36 mm across → focal giving the same vertical view.
        let vap = 36.0 * 1080.0 / 1920.0;
        let focal = vap / 2.0 / (vfov / 2.0).tan();
        assert!(nk.contains(&format!("focal {focal:.6}")));
        assert_eq!(obj(&ct, &[]).lines().filter(|l| l.starts_with("v ")).count(), 2);
        assert_eq!(obj(&ct, &[1]).lines().filter(|l| l.starts_with("v ")).count(), 1);
    }

    #[test]
    fn blender_and_after_effects_scripts() {
        let ct = track();
        let py = blender(&ct, "Shot \"1\"", &[]);
        assert!(py.contains("NAME = \"Shot \\\"1\\\"\""), "names are quoted safely");
        assert_eq!(py.matches("),\n").count() >= 32, true);
        assert!(py.contains("cam_data.angle_y = math.radians(FOV)") && py.contains("FOV = 42.000000"));
        assert!(py.contains("scene.render.resolution_x = 1920"));
        // Blender's axes: the first camera's back axis is the editor's forward turned (Z up, Y away), negated.
        let (_, _, f, _) = axes(&ct, 0);
        let back = dir_blender(neg(f));
        assert!(py.contains(&py_v(back)));
        // After Effects: the comp's top-left origin, every frame keyed, a null per point.
        let jsx = after_effects(&ct, "Shot", &[]);
        let c0 = &ct.cameras[0];
        assert!(jsx.contains(&format!("[{:.4},{:.4},{:.4}]", c0.position[0] as f64 + 960.0, c0.position[1] as f64 + 540.0, c0.position[2])));
        assert!(jsx.contains("addComp(\"Shot (camera)\", 1920, 1080, 1, Math.max(30 / fps"));
        assert!(jsx.contains(&format!("setValue({:.4})", ct.camera(0).focal())));
        assert!(jsx.contains("var pts = [[1,") && jsx.contains("[2,["));
        assert_eq!(jsx.matches("setValuesAtTimes").count(), 3);
        assert!((track_fps(&ct) - 24.0).abs() < 1e-6);
    }

    #[test]
    fn json_round_trip_and_checks() {
        let mut ct = track();
        ct.camera = Some(crate::project::ClipId(5));
        let back = read_json(&json(&ct)).unwrap();
        assert_eq!(back.camera, None, "the camera clip is this project's, not the file's");
        assert_eq!(back.cameras, ct.cameras);
        assert_eq!(back.points, ct.points);
        assert!(read_json("{}").is_err());
        let mut bad = ct.clone();
        bad.times.pop();
        assert!(read_json(&json(&bad)).is_err());
    }

    #[test]
    fn chan_times_follow_the_files_frames() {
        let path = ChanPath { frames: vec![(101, track().cameras[0]), (102, track().cameras[1]), (104, track().cameras[2])], fov: 40.0 };
        let t = chan_times(&path, Time::from_secs_f64(2.0), 25.0);
        assert_eq!(t[0], Time::from_secs_f64(2.0));
        assert_eq!(t[2], Time::from_secs_f64(2.0) + Time::from_secs_f64(3.0 / 25.0));
    }
}
