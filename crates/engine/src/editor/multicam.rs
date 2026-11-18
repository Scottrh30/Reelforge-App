//! Multicam editing: make a multicam source from several takes (lined up
//! by their sound or their starts), then cut between its angles at the
//! playhead, live while it plays (number keys, or a click on the Viewer's
//! angle grid). See `reelforge_core::multicam`.

use super::Editor;
use reelforge_core::multicam::{self, Angle, MulticamSync};
use reelforge_core::undo::{Batch, Command, SetAngle, SplitClip};
use reelforge_core::{ClipId, MediaId};

/// The multicam clip at the playhead, for the Viewer and the number keys.
#[derive(Clone, Debug, PartialEq)]
pub struct MulticamAt {
    pub clip: ClipId,
    /// The multicam source (sequence index).
    pub source: usize,
    pub angle: Angle,
    /// The angles' names, in order.
    pub angles: Vec<String>,
}

impl Editor {
    /// Make a multicam source from `media` (the takes, in angle order), put
    /// a clip playing it at the playhead, and show the angle grid.
    pub fn create_multicam(&mut self, media: &[MediaId], sync: MulticamSync) {
        self.commit_live_edit();
        let offsets = match sync {
            MulticamSync::Start => vec![0.0; media.len()],
            MulticamSync::Audio => {
                let envs: Vec<Vec<f32>> = media.iter().map(|m| self.peaks_now(*m).map(|p| p.to_vec()).unwrap_or_default()).collect();
                let refs: Vec<&[f32]> = envs.iter().map(|e| e.as_slice()).collect();
                multicam::sync_offsets(&refs, super::WAVEFORM_RATE)
            }
        };
        let fallback = self.seq().clone();
        match reelforge_core::undo::multicam_from_media(&mut self.project, media, &offsets, &fallback) {
            Ok(cmd) => {
                let (index, name) = (cmd.index, cmd.seq.name.clone());
                self.execute(Box::new(Batch { name: "New multicam source".into(), cmds: vec![Box::new(cmd)] }));
                self.add_composite_to_timeline(index);
                self.set_multicam_grid(true);
                let how = match sync {
                    MulticamSync::Audio => "lined up by sound",
                    MulticamSync::Start => "lined up by their starts",
                };
                self.status = format!("{name}: {} angles, {how}. Press 1–9 to cut to an angle", media.len());
            }
            Err(e) => self.status = e,
        }
    }

    /// The multicam clip under the playhead: the selected one, else the top one.
    pub fn multicam_at(&self) -> Option<MulticamAt> {
        let t = self.playback.playhead;
        let s = self.seq();
        let is_mc = |c: &reelforge_core::Clip| c.angle.is_some() && c.nested.and_then(|i| self.project.sequences.get(i)).is_some_and(|s| s.multicam);
        let video = |c: &&reelforge_core::Clip| s.find_clip(c.id).is_some_and(|(ti, _)| s.tracks[ti].kind == reelforge_core::TrackKind::Video);
        let all = || s.tracks.iter().flat_map(|tr| &tr.clips).filter(|c| c.contains(t) && is_mc(c));
        let clip = self.selection.and_then(|id| all().find(|c| c.id == id)).or_else(|| {
            // The top video track's first, else any (an audio-only one).
            let mut v: Vec<_> = all().filter(video).collect();
            v.sort_by_key(|c| std::cmp::Reverse(s.find_clip(c.id).map_or(0, |(ti, _)| ti)));
            v.first().copied().or_else(|| all().next())
        })?;
        let source = clip.nested?;
        Some(MulticamAt { clip: clip.id, source, angle: clip.angle?, angles: multicam::angle_names(&self.project.sequences[source]) })
    }

    /// Cut the multicam clip at the playhead to angle `video` from here on:
    /// picture and sound, or (`picture_only`) the picture alone, the sound
    /// staying on the angle heard now. At a clip's first frame it switches
    /// the whole clip instead of cutting.
    pub fn switch_angle(&mut self, video: u8, picture_only: bool) {
        self.commit_live_edit();
        let Some(at) = self.multicam_at() else {
            self.status = "No multicam clip at the playhead".into();
            return;
        };
        if video as usize >= at.angles.len() {
            self.status = format!("There's no angle {} (it has {})", video + 1, at.angles.len());
            return;
        }
        let angle = Angle { video, audio: picture_only.then(|| at.angle.heard()) };
        let name = &at.angles[video as usize];
        if self.seq().is_locked(at.clip) {
            self.status = "The clip is on a locked track".into();
            return;
        }
        let s = self.seq();
        let t = self.playback.playhead.snap(s.frame_rate);
        let Some((_, c)) = s.find_clip(at.clip) else { return };
        let mut cmds: Vec<Box<dyn Command>> = Vec::new();
        let targets: Vec<ClipId>;
        if t <= c.timeline_start || t >= c.timeline_end() {
            if at.angle == angle {
                return;
            }
            targets = s.link_group(at.clip).iter().map(|(_, c)| c.id).collect();
        } else {
            if at.angle == angle {
                self.status = format!("Already on {name}");
                return;
            }
            // The halves after the cut get new ids (and a link of their own).
            let group: Vec<ClipId> = s.link_group(at.clip).iter().filter(|(_, c)| c.contains(t) && c.timeline_start < t).map(|(_, c)| c.id).collect();
            let link = Some(self.project.alloc_id());
            let mut right = Vec::new();
            for id in group {
                let new_id = ClipId(self.project.alloc_id());
                let mut split = SplitClip::new(self.active, id, t, new_id);
                split.new_link = link;
                cmds.push(Box::new(split));
                right.push(new_id);
            }
            targets = right;
        }
        cmds.push(Box::new(SetAngle { seq: self.active, clips: targets.into_iter().map(|id| (id, Some(angle))).collect() }));
        self.execute(Box::new(Batch { name: format!("Cut to {name}"), cmds }));
        self.status = format!("Cut to {name}{}", if picture_only { " (picture only)" } else { "" });
    }

    /// Hear angle `audio` on multicam clip `clip` (and the clips linked to
    /// it); `None`: the angle seen.
    pub fn set_audio_angle(&mut self, clip: ClipId, audio: Option<u8>) {
        self.commit_live_edit();
        let clips: Vec<(ClipId, Option<Angle>)> = self.seq().link_group(clip).iter().filter_map(|(_, c)| c.angle.map(|a| (c.id, Some(Angle { audio, ..a })))).collect();
        if clips.is_empty() {
            return;
        }
        self.execute(Box::new(SetAngle { seq: self.active, clips }));
    }

    /// Show multicam clips in the Viewer as a grid of their angles.
    pub fn set_multicam_grid(&mut self, on: bool) {
        if self.multicam_grid != on {
            self.multicam_grid = on;
            self.view_rev += 1;
        }
        multicam::set_grid(on);
    }

    /// The angle (0-based) whose cell of the grid is at `p`, in frame pixels
    /// from the top left of the sequence's picture.
    pub fn grid_angle_at(&self, p: [f32; 2]) -> Option<u8> {
        let at = self.multicam_at()?;
        let s = self.seq();
        multicam::angle_at(p, at.angles.len(), [s.width as f32, s.height as f32]).map(|i| i as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::{MediaAsset, Time, TrackKind};
    use std::path::Path;

    /// A take of the "event": a flat-colour picture and a tone with gaps at
    /// uneven moments, starting `from` seconds in.
    fn take(dir: &Path, name: &str, colour: &str, from: f64) -> Option<std::path::PathBuf> {
        let p = dir.join(name);
        let gaps = "between(t,1,1.3)+between(t,2.7,2.9)+between(t,4.1,4.6)+between(t,6.6,6.7)+between(t,7.2,7.9)";
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", &format!("color=c={colour}:size=64x36:rate=25:duration=9"), "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration=10,volume=enable='{gaps}':volume=0,atrim=start={from}"))
            .args(["-t", "6", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(&p)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(p)
    }

    /// Two takes lined up by their sound (B started 1.5 s later), cut
    /// between at the playhead, picture and sound or picture only: each cut
    /// shows and plays only its angle, the grid shows both, and undo puts
    /// it back.
    #[test]
    fn takes_line_up_by_sound_and_cut_between_angles() {
        let dir = std::env::temp_dir().join(format!("rf-multicam-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (Some(a), Some(b)) = (take(&dir, "a.mp4", "red", 0.0), take(&dir, "b.mp4", "blue", 1.5)) else { return };
        let mut e = Editor::new(reelforge_render::headless_device(), None);
        // The edit the takes' size, so the multicam clip fills it.
        (e.project.sequences[0].width, e.project.sequences[0].height) = (64, 36);
        for (id, p) in [(1, &a), (2, &b)] {
            let info = reelforge_media::probe(p).unwrap();
            e.project.media.push(MediaAsset { id: MediaId(id), path: p.clone(), name: p.file_name().unwrap().to_string_lossy().into(), info, conform: None });
        }
        e.create_multicam(&[MediaId(1)], MulticamSync::Audio);
        assert!(e.status.contains("two or more"), "{}", e.status);
        e.create_multicam(&[MediaId(1), MediaId(2)], MulticamSync::Audio);
        let src = e.project.sequences.len() - 1;
        let s = &e.project.sequences[src];
        assert!(s.multicam && e.multicam_grid);
        assert_eq!(multicam::angle_names(s), ["1 a", "2 b"]);
        // B's take starts 1.5 s into A's (to the frame).
        let starts: Vec<Time> = multicam::video_angles(s).iter().map(|t| s.tracks[*t].clips[0].timeline_start).collect();
        assert_eq!(starts[0], Time::ZERO);
        assert!((starts[1].as_secs_f64() - 1.5).abs() < 0.045, "{starts:?}");
        assert_eq!(multicam::audio_angles(s).iter().map(|t| s.tracks[*t].clips.len()).collect::<Vec<_>>(), [1, 1]);

        // On the timeline: a linked picture and sound clip on angle 1.
        let at = e.multicam_at().expect("at the playhead");
        assert_eq!((at.source, at.angle), (src, Angle::default()));
        let clip = at.clip;
        assert_eq!(e.seq().link_group(clip).len(), 2);

        // Cut to angle 2 at 2 s, then picture-only back to angle 1 at 3 s.
        e.playback.seek(Time::from_secs_f64(2.0));
        e.switch_angle(1, false);
        e.playback.seek(Time::from_secs_f64(3.0));
        e.switch_angle(0, true);
        e.switch_angle(5, false);
        assert!(e.status.contains("no angle 6"), "{}", e.status);
        let angles = |e: &Editor, kind: TrackKind| -> Vec<(f64, Angle)> {
            let s = e.seq();
            s.tracks.iter().filter(|t| t.kind == kind).flat_map(|t| &t.clips).filter(|c| c.angle.is_some()).map(|c| (c.timeline_start.as_secs_f64(), c.angle.unwrap())).collect()
        };
        let want = vec![(0.0, Angle { video: 0, audio: None }), (2.0, Angle { video: 1, audio: None }), (3.0, Angle { video: 0, audio: Some(1) })];
        assert_eq!(angles(&e, TrackKind::Video), want);
        assert_eq!(angles(&e, TrackKind::Audio), want, "the sound is cut with the picture");

        // What's seen: angle 1 is red, angle 2 blue; the grid has both.
        let colour = |e: &Editor, t: f64| -> [u8; 3] {
            let (device, queue) = e.preview.gpu().unwrap();
            let (rgba, [w, h]) = reelforge_export::render_still(&e.project, e.active, Time::from_secs_f64(t), &device, &queue).unwrap();
            let px = |x: u32, y: u32| {
                let i = ((y * w + x) * 4) as usize;
                [rgba[i], rgba[i + 1], rgba[i + 2]]
            };
            let _ = h;
            px(w / 4, h / 2)
        };
        if e.preview.gpu().is_some() {
            multicam::set_grid(false);
            let red = |c: [u8; 3]| c[0] > 180 && c[2] < 80;
            let blue = |c: [u8; 3]| c[2] > 180 && c[0] < 80;
            assert!(red(colour(&e, 1.0)), "{:?}", colour(&e, 1.0));
            assert!(blue(colour(&e, 2.5)), "{:?}", colour(&e, 2.5));
            assert!(red(colour(&e, 3.5)));
            // The grid: angle 1 top left, angle 2 top right (side by side for two).
            multicam::set_grid(true);
            let (device, queue) = e.preview.gpu().unwrap();
            let (rgba, [w, h]) = reelforge_export::render_still(&e.project, e.active, Time::from_secs_f64(2.5), &device, &queue).unwrap();
            let px = |x: u32, y: u32| {
                let i = ((y * w + x) * 4) as usize;
                [rgba[i], rgba[i + 1], rgba[i + 2]]
            };
            assert!(red(px(w / 4, h / 2)) && blue(px(3 * w / 4, h / 2)), "{:?} {:?}", px(w / 4, h / 2), px(3 * w / 4, h / 2));
            multicam::set_grid(false);
            assert_eq!(e.grid_angle_at([e.seq().width as f32 * 0.8, 10.0]), Some(1));
        }

        // What's heard: only one angle at a time.
        let mix = reelforge_export::mix_clips(&e.project, e.active, 48000);
        let mut heard: Vec<(f64, std::path::PathBuf)> = mix.iter().map(|m| ((m.start as f64 / 48000.0 * 10.0).round() / 10.0, m.path.clone())).collect();
        heard.sort_by(|x, y| x.0.total_cmp(&y.0));
        assert_eq!(heard, [(0.0, a.clone()), (2.0, b.clone()), (3.0, b.clone())], "picture-only keeps angle 2's sound");

        for _ in 0..2 {
            e.undo();
        }
        assert_eq!(angles(&e, TrackKind::Video), [(0.0, Angle::default())]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
