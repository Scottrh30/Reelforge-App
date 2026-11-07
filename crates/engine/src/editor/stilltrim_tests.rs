//! Lengthening stills on the timeline (see `Editor::trim_live`).

#[cfg(test)]
mod tests {
    use crate::editor::Editor;
    use reelforge_core::undo::Edge;
    use reelforge_core::*;

    fn asset(id: u64, secs: f64) -> MediaAsset {
        MediaAsset {
            id: MediaId(id),
            path: format!("/x/{id}").into(),
            name: String::new(),
            info: MediaInfo {
                duration: Time::from_secs_f64(secs),
                video: Some(VideoStreamInfo { width: 64, height: 64, frame_rate: FrameRate::new(25, 1), codec: "png".into(), time_base: Rational::new(1, 25), hdr: None }),
                audio: None,
                container: String::new(),
            },
            conform: None,
        }
    }

    /// Two pictures side by side: pulling the first one's end out pushes the
    /// second along (and undoes as one step); a video clip in the same spot
    /// stops at its neighbour and says how to push it.
    #[test]
    fn pulling_a_still_longer_pushes_the_next_clip() {
        let s = Time::from_secs_f64;
        let mut e = Editor::new(None, None);
        e.project.media = vec![asset(1, 0.0), asset(2, 0.0), asset(3, 10.0)];
        let seq = &mut e.project.sequences[e.active];
        seq.tracks[0].clips = vec![Clip::new(ClipId(10), MediaId(1), s(0.0), s(0.0), s(5.0), None), Clip::new(ClipId(11), MediaId(2), s(5.0), s(0.0), s(5.0), None)];
        e.trim_live(ClipId(10), Edge::End, crate::editor::TrimMode::Normal, s(3.0));
        e.commit_live_edit();
        let c = |e: &Editor, id| e.seq().find_clip(ClipId(id)).unwrap().1.clone();
        assert_eq!(c(&e, 10).duration, s(8.0));
        assert_eq!(c(&e, 11).timeline_start, s(8.0), "the next picture moved along");
        e.undo();
        assert_eq!((c(&e, 10).duration, c(&e, 11).timeline_start), (s(5.0), s(5.0)));
        // Shorter: a plain trim (it leaves a gap, as before).
        e.trim_live(ClipId(10), Edge::End, crate::editor::TrimMode::Normal, s(-1.0));
        e.commit_live_edit();
        assert_eq!((c(&e, 10).duration, c(&e, 11).timeline_start), (s(4.0), s(5.0)));
        e.undo();

        // A video clip (3 s used of 10) in the first place: stopped by its neighbour.
        e.project.sequences[e.active].tracks[0].clips[0] = Clip::new(ClipId(10), MediaId(3), s(0.0), s(0.0), s(5.0), None);
        e.trim_live(ClipId(10), Edge::End, crate::editor::TrimMode::Normal, s(2.0));
        e.commit_live_edit();
        assert_eq!(c(&e, 10).duration, s(5.0));
        assert!(e.status.contains("Ripple tool"), "{}", e.status);
    }
}
