//! Paste attributes: copy one clip, then put chosen parts of it (its
//! transform, crop, corner pin, blend mode, effects, masks, audio levels or
//! text style) onto other clips, keeping theirs otherwise. The same colour
//! grade on every shot, say.
//!
//! Keyframes come along at the same distance from the clip's start (keys are
//! stored in each clip's own time, so they're shifted by the difference
//! between the two clips' source-in points). Masks are in the layer's own
//! pixels, so they're scaled when the target picture is a different size.

use crate::anim::{Curve, Lerp, Prop};
use crate::audio::ClipAudio;
use crate::clipboard::Clipboard;
use crate::project::{Clip, ClipId, Project, TrackKind};
use crate::time::Time;
use crate::undo::{expand_links, Batch, Command, SetClipAudio, SetClipVisual, SetTitle};
use serde::{Deserialize, Serialize};

/// Which attributes to paste.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AttrSet {
    /// Position, scale, rotation, anchor point and opacity (with their keyframes).
    pub transform: bool,
    pub crop: bool,
    pub corner_pin: bool,
    pub blend: bool,
    pub effects: bool,
    /// With `effects`: replace the target's effects (rather than add after them).
    pub replace_effects: bool,
    pub masks: bool,
    /// Volume (with keyframes), pan and fades.
    pub audio: bool,
    /// Font, size, colour, outline, shadow… of a title (its text is kept).
    pub text_style: bool,
}

impl Default for AttrSet {
    fn default() -> Self {
        Self { transform: false, crop: false, corner_pin: false, blend: false, effects: true, replace_effects: true, masks: false, audio: false, text_style: false }
    }
}

impl AttrSet {
    pub fn any(&self) -> bool {
        self.transform || self.crop || self.corner_pin || self.blend || self.effects || self.masks || self.audio || self.text_style
    }
}

/// What the clipboard has to offer: the first copied video clip and audio clip.
pub struct Source<'a> {
    pub video: Option<&'a Clip>,
    pub audio: Option<&'a Clip>,
}

impl<'a> Source<'a> {
    pub fn of(cb: &'a Clipboard) -> Self {
        let first = |k: TrackKind| cb.clips.iter().find(|c| c.kind == k).map(|c| &c.clip);
        Source { video: first(TrackKind::Video), audio: first(TrackKind::Audio) }
    }

    /// The attributes there's something to paste from (each with a short note
    /// for the dialog), and the clip they come from.
    pub fn offers(&self) -> Offers {
        let v = self.video;
        Offers {
            visual: v.is_some(),
            corner_pin: v.is_some_and(|c| c.transform.pin.is_some() || c.anim.pin.is_animated()),
            effects: v.map_or(0, |c| c.effects.len()),
            masks: v.map_or(0, |c| c.masks.len()),
            audio: self.audio.is_some(),
            text_style: v.is_some_and(|c| c.title.is_some()),
        }
    }
}

/// See [`Source::offers`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offers {
    pub visual: bool,
    pub corner_pin: bool,
    pub effects: usize,
    pub masks: usize,
    pub audio: bool,
    pub text_style: bool,
}

fn shift_curve<T: Lerp>(c: &Curve<T>, by: Time) -> Curve<T> {
    let mut c = c.clone();
    for k in &mut c.0 {
        k.at = k.at + by;
    }
    c
}

/// Paste `set` from the clipboard's clips onto `targets` (and the clips linked
/// to them) in sequence `seq`: visual attributes onto video clips, audio
/// levels onto audio clips, text style onto titles. Locked clips are left
/// alone. Returns one undo step and how many clips it changes.
pub fn paste_attributes(p: &Project, seq: usize, cb: &Clipboard, targets: &[ClipId], set: AttrSet) -> Result<(Box<dyn Command>, usize), &'static str> {
    if cb.is_empty() {
        return Err("copy a clip first (Ctrl+C), then select the clips to paste onto");
    }
    if !set.any() {
        return Err("nothing chosen to paste");
    }
    if targets.is_empty() {
        return Err("select the clips to paste onto first");
    }
    let src = Source::of(cb);
    let s = &p.sequences[seq];
    let frame = [s.width as f32, s.height as f32];
    // A layer's own pixel size (masks are in it): its picture, or the frame for titles.
    let size = |c: &Clip| p.layer_dims(c, s).unwrap_or(frame);
    let mut cmds: Vec<Box<dyn Command>> = Vec::new();
    let mut changed = 0;
    let mut locked = 0;
    for id in expand_links(p, seq, targets) {
        let Some((ti, c)) = s.find_clip(id) else { continue };
        let kind = s.tracks[ti].kind;
        if s.tracks[ti].locked {
            locked += 1;
            continue;
        }
        let mut touched = false;
        if kind == TrackKind::Video {
            if let Some(from) = src.video {
                let by = c.source_in - from.source_in;
                let old = c.visual();
                let mut v = old.clone();
                let mut props: Vec<Prop> = Vec::new();
                if set.transform {
                    props.extend([Prop::Position, Prop::Scale, Prop::Rotation, Prop::Anchor, Prop::Opacity, Prop::Depth, Prop::Orientation]);
                    v.transform.three_d = from.transform.three_d;
                    v.transform.lit = from.transform.lit;
                }
                if set.crop {
                    props.push(Prop::Crop);
                }
                if set.corner_pin {
                    props.push(Prop::Pin);
                }
                for prop in props {
                    v.transform.copy_prop(prop, &from.transform);
                    match prop {
                        Prop::Position => v.anim.position = shift_curve(&from.anim.position, by),
                        Prop::Scale => v.anim.scale = shift_curve(&from.anim.scale, by),
                        Prop::Rotation => v.anim.rotation = shift_curve(&from.anim.rotation, by),
                        Prop::Opacity => v.anim.opacity = shift_curve(&from.anim.opacity, by),
                        Prop::Anchor => v.anim.anchor = shift_curve(&from.anim.anchor, by),
                        Prop::Crop => v.anim.crop = shift_curve(&from.anim.crop, by),
                        Prop::Pin => v.anim.pin = shift_curve(&from.anim.pin, by),
                        Prop::Depth => v.anim.depth = shift_curve(&from.anim.depth, by),
                        Prop::Orientation => v.anim.orientation = shift_curve(&from.anim.orientation, by),
                    }
                }
                if set.blend {
                    v.blend = from.blend;
                }
                if set.effects && !from.effects.is_empty() {
                    let fx = from.effects.iter().cloned().map(|mut e| {
                        for prm in &mut e.params {
                            prm.keys = shift_curve(&prm.keys, by);
                        }
                        e
                    });
                    if set.replace_effects {
                        v.effects = fx.collect();
                    } else {
                        v.effects.extend(fx);
                    }
                }
                if set.masks && !from.masks.is_empty() {
                    let ([sw, sh], [tw, th]) = (size(from), size(c));
                    let (kx, ky) = (tw / sw.max(1.0), th / sh.max(1.0));
                    let scale = |pts: &mut Vec<[f32; 2]>| pts.iter_mut().for_each(|q| *q = [q[0] * kx, q[1] * ky]);
                    v.masks = from
                        .masks
                        .iter()
                        .cloned()
                        .map(|mut m| {
                            scale(&mut m.points);
                            for k in &mut m.keys {
                                k.at = k.at + by;
                                scale(&mut k.points);
                            }
                            m
                        })
                        .collect();
                }
                if v != old {
                    cmds.push(Box::new(SetClipVisual { seq, id, old, new: v }));
                    touched = true;
                }
            }
            if let (true, Some(from), Some(old)) = (set.text_style, src.video.and_then(|f| f.title.as_ref()), c.title.as_ref()) {
                let new = crate::title::TitleSpec { text: old.text.clone(), ..from.clone() };
                if new != *old {
                    cmds.push(Box::new(SetTitle { seq, id, old: old.clone(), new }));
                    touched = true;
                }
            }
        } else if let (true, Some(from)) = (set.audio, src.audio) {
            let by = c.source_in - from.source_in;
            let old = c.audio.clone();
            let mut a = ClipAudio { crossfade_in: old.crossfade_in, ..from.audio.clone() };
            for k in &mut a.volume_keys {
                k.at = k.at + by;
            }
            for k in &mut a.pan_keys {
                k.at = k.at + by;
            }
            // Fades can't be longer than this clip.
            a.fade_in = a.fade_in.min(c.duration);
            a.fade_out = a.fade_out.min(c.duration - a.fade_in);
            if a != old {
                cmds.push(Box::new(SetClipAudio { seq, id, old, new: a }));
                touched = true;
            }
        }
        if touched {
            changed += 1;
        }
    }
    if cmds.is_empty() {
        return Err(if locked > 0 { "those clips are on locked tracks" } else { "nothing to change: the selected clips already match, or aren't the right kind" });
    }
    Ok((Box::new(Batch { name: if changed > 1 { "Paste attributes to clips" } else { "Paste attributes" }.into(), cmds }), changed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::copy_clips;
    use crate::effects::{Effect, EffectKind};
    use crate::project::{MediaId, Sequence};
    use crate::transform::{BlendMode, Pin};
    use crate::undo::UndoStack;
    use crate::FrameRate;

    fn s(x: f64) -> Time {
        Time::from_secs_f64(x)
    }

    fn project() -> Project {
        let mut p = Project::new();
        let mut seq = Sequence::new("T", 1920, 1080, FrameRate::new(25, 1));
        // Source: 2 s into its media, graded and keyed.
        let mut a = Clip::new(ClipId(1), MediaId::GENERATED, s(0.0), s(2.0), s(4.0), Some(7));
        a.transform.position = [100.0, -50.0];
        a.anim.rotation.set(s(3.0), 45.0); // 1 s after its start
        a.transform.crop = [0.1, 0.0, 0.0, 0.0];
        a.blend = BlendMode::Screen;
        let mut cc = Effect::new(EffectKind::ColorCorrect);
        cc.params[0].keys.set(s(2.5), 0.3);
        a.effects.push(cc);
        a.masks.push(crate::mask::Mask { points: vec![[0.0, 0.0], [1920.0, 0.0], [1920.0, 1080.0]], ..Default::default() });
        let mut aa = Clip::new(ClipId(2), MediaId::GENERATED, s(0.0), s(2.0), s(4.0), Some(7));
        aa.audio.volume_db = -6.0;
        aa.audio.fade_in = s(1.0);
        aa.audio.volume_keys.push(crate::audio::VolumeKey { at: s(2.5), db: -12.0 });
        // Target: from 0 s of its media, with an effect of its own.
        let mut b = Clip::new(ClipId(3), MediaId::GENERATED, s(5.0), s(0.0), s(3.0), Some(8));
        b.effects.push(Effect::new(EffectKind::Vignette));
        let mut bb = Clip::new(ClipId(4), MediaId::GENERATED, s(5.0), s(0.0), s(3.0), Some(8));
        bb.audio.crossfade_in = s(0.5);
        seq.tracks[0].clips = vec![a, b];
        seq.tracks[2].clips = vec![aa, bb];
        p.sequences = vec![seq];
        p
    }

    #[test]
    fn pastes_the_chosen_attributes_keeping_keys_in_place() {
        let mut p = project();
        let cb = copy_clips(&p, 0, &[ClipId(1)]);
        let set = AttrSet { transform: true, effects: true, replace_effects: true, ..AttrSet::default() };
        let (cmd, n) = paste_attributes(&p, 0, &cb, &[ClipId(3)], set).unwrap();
        assert_eq!(n, 1, "the audio partner isn't touched without Audio levels");
        let mut u = UndoStack::default();
        u.execute(&mut p, cmd);
        let b = p.sequences[0].find_clip(ClipId(3)).unwrap().1.clone();
        assert_eq!(b.transform.position, [100.0, -50.0]);
        // The rotation key was 1 s after the source's start: still 1 s after this one's.
        assert_eq!(b.anim.rotation.0[0].at, s(1.0));
        assert_eq!(b.effects.len(), 1, "replaced");
        assert_eq!(b.effects[0].kind, EffectKind::ColorCorrect);
        assert_eq!(b.effects[0].params[0].keys.0[0].at, s(0.5));
        // Not chosen: crop and blend stay.
        assert_eq!((b.transform.crop, b.blend), ([0.0; 4], BlendMode::Normal));
        u.undo(&mut p);
        let b = p.sequences[0].find_clip(ClipId(3)).unwrap().1.clone();
        assert_eq!((b.transform.position, b.effects[0].kind), ([0.0, 0.0], EffectKind::Vignette));
    }

    #[test]
    fn adds_effects_scales_masks_and_pastes_audio_levels() {
        let mut p = project();
        // The target's picture is half the size of the source's (titles use the frame: make it a 960×540 composite).
        let mut small = Sequence::new("Small", 960, 540, FrameRate::new(25, 1));
        small.tracks.clear();
        p.sequences.push(small);
        p.sequences[0].find_clip_mut(ClipId(3)).unwrap().1.nested = Some(1);
        let cb = copy_clips(&p, 0, &[ClipId(1)]);
        let set = AttrSet { effects: true, replace_effects: false, masks: true, audio: true, ..AttrSet::default() };
        let (cmd, n) = paste_attributes(&p, 0, &cb, &[ClipId(3)], set).unwrap();
        assert_eq!(n, 2, "the video and its linked audio");
        cmd_apply(&mut p, cmd);
        let b = p.sequences[0].find_clip(ClipId(3)).unwrap().1.clone();
        assert_eq!(b.effects.iter().map(|e| e.kind).collect::<Vec<_>>(), [EffectKind::Vignette, EffectKind::ColorCorrect]);
        assert_eq!(b.masks[0].points[2], [960.0, 540.0]);
        let bb = p.sequences[0].find_clip(ClipId(4)).unwrap().1.clone();
        assert_eq!((bb.audio.volume_db, bb.audio.fade_in), (-6.0, s(1.0)));
        assert_eq!(bb.audio.volume_keys[0].at, s(0.5));
        assert_eq!(bb.audio.crossfade_in, s(0.5), "its crossfade with the clip before is its own");
    }

    fn cmd_apply(p: &mut Project, mut c: Box<dyn Command>) {
        c.apply(p);
    }

    #[test]
    fn corner_pins_text_styles_and_refusals() {
        let mut p = project();
        let pin = Pin([[10.0, 10.0], [200.0, 20.0], [190.0, 150.0], [15.0, 140.0]]);
        p.sequences[0].find_clip_mut(ClipId(1)).unwrap().1.transform.pin = Some(pin);
        let cb = copy_clips(&p, 0, &[ClipId(1)]);
        assert!(Source::of(&cb).offers().corner_pin);
        let (cmd, _) = paste_attributes(&p, 0, &cb, &[ClipId(3)], AttrSet { corner_pin: true, effects: false, ..AttrSet::default() }).unwrap();
        cmd_apply(&mut p, cmd);
        assert_eq!(p.sequences[0].find_clip(ClipId(3)).unwrap().1.transform.pin, Some(pin));

        // Titles: style comes over, the text stays.
        let mut t1 = Clip::new(ClipId(10), MediaId::GENERATED, s(20.0), s(0.0), s(2.0), None);
        t1.title = Some(crate::title::TitleSpec { text: "Source".into(), size: 120.0, color: [255, 0, 0, 255], ..Default::default() });
        let mut t2 = Clip::new(ClipId(11), MediaId::GENERATED, s(30.0), s(0.0), s(2.0), None);
        t2.title = Some(crate::title::TitleSpec { text: "Mine".into(), ..Default::default() });
        p.sequences[0].tracks[1].clips = vec![t1, t2];
        let cb = copy_clips(&p, 0, &[ClipId(10)]);
        assert!(Source::of(&cb).offers().text_style);
        let (cmd, _) = paste_attributes(&p, 0, &cb, &[ClipId(11)], AttrSet { text_style: true, effects: false, ..AttrSet::default() }).unwrap();
        cmd_apply(&mut p, cmd);
        let t = p.sequences[0].find_clip(ClipId(11)).unwrap().1.title.clone().unwrap();
        assert_eq!((t.text.as_str(), t.size, t.color), ("Mine", 120.0, [255, 0, 0, 255]));

        // Refusals.
        assert!(paste_attributes(&p, 0, &Clipboard::default(), &[ClipId(3)], AttrSet::default()).is_err());
        assert!(paste_attributes(&p, 0, &cb, &[], AttrSet::default()).is_err());
        p.sequences[0].tracks[0].locked = true;
        let cb = copy_clips(&p, 0, &[ClipId(1)]);
        assert_eq!(paste_attributes(&p, 0, &cb, &[ClipId(3)], AttrSet::default()).err(), Some("those clips are on locked tracks"));
    }
}
