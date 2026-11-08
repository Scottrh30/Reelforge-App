//! The Paint effect in the editor: strokes painted in the Viewer.

use super::{Editor, LOCKED};
use reelforge_core::effects::{paint_params as pp, EffectData};
use reelforge_core::paint::{Lasts, PaintStroke, Tool};
use reelforge_core::*;

impl Editor {
    /// Add a stroke through `points` (layer pixels) to Paint effect `index`
    /// of clip `id`, at the playhead, with the effect's brush settings. With
    /// `source` (an Alt-click, layer pixels), clone strokes copy from there
    /// from now on: the offset is set from it to the stroke's start. One undo step.
    pub fn paint_stroke(&mut self, id: ClipId, index: usize, points: Vec<[f32; 2]>, source: Option<[f32; 2]>) {
        self.commit_live_edit();
        if self.seq().is_locked(id) {
            self.status = LOCKED.into();
            return;
        }
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        if points.is_empty() {
            return;
        }
        let at = self.mask_time(c);
        let frame = self.seq().frame_rate.frame_duration();
        let Some(mut v) = self.visual(id) else { return };
        let Some(fx) = v.effects.get_mut(index).filter(|f| f.kind == EffectKind::Paint) else { return };
        if let Some(s) = source {
            fx.params[pp::OFFSET_X].value = s[0] - points[0][0];
            fx.params[pp::OFFSET_Y].value = s[1] - points[0][1];
        }
        let val = |i: usize| fx.value_at(i, at);
        let tool = match val(pp::TOOL).round() as i32 {
            1 => Tool::Brush,
            2 => Tool::Erase,
            _ => Tool::Clone,
        };
        let lasts = match val(pp::LASTS).round() as i32 {
            1 => Lasts::OnFromHere,
            2 => Lasts::Always,
            _ => Lasts::Frame,
        };
        let stroke = PaintStroke {
            tool,
            points,
            radius: val(pp::SIZE).max(0.5) / 2.0,
            hardness: val(pp::HARDNESS),
            opacity: val(pp::OPACITY),
            colour: val(pp::COLOUR).round().clamp(0.0, 16777215.0) as u32,
            offset: [val(pp::OFFSET_X), val(pp::OFFSET_Y)],
            at,
            lasts,
            frame,
        };
        let mut strokes = match fx.data.take() {
            Some(EffectData::Paint(s)) => s,
            _ => Vec::new(),
        };
        strokes.push(stroke);
        let n = strokes.len();
        fx.data = Some(EffectData::Paint(strokes));
        self.set_visual_live(id, v);
        self.commit_live_edit();
        let what = match tool {
            Tool::Clone => "Cloned",
            Tool::Brush => "Painted",
            Tool::Erase => "Erased",
        };
        let how = match lasts {
            Lasts::Frame => "on this frame",
            Lasts::OnFromHere => "from this frame on",
            Lasts::Always => "for the whole clip",
        };
        self.status = format!("{what} {how} ({n} stroke{}; undo takes the last one away)", if n == 1 { "" } else { "s" });
    }

    /// Take every stroke off Paint effect `index` of clip `id` (one undo step).
    pub fn paint_clear(&mut self, id: ClipId, index: usize) {
        self.commit_live_edit();
        let Some(mut v) = self.visual(id) else { return };
        let Some(fx) = v.effects.get_mut(index).filter(|f| f.kind == EffectKind::Paint) else { return };
        fx.data = None;
        self.set_visual_live(id, v);
        self.commit_live_edit();
        self.status = "Strokes cleared".into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strokes_take_the_brush_settings_and_alt_click_sets_the_clone_source() {
        let mut e = Editor::new(None, None);
        let id = ClipId(e.project.alloc_id());
        let mut c = Clip::new_plane(id, Time::ZERO, Time::from_secs_f64(2.0), [10, 20, 30, 255]);
        let mut fx = Effect::new(EffectKind::Paint);
        fx.params[pp::LASTS].value = 2.0;
        c.effects.push(fx);
        let v = e.seq().tracks.iter().position(|tr| tr.kind == TrackKind::Video).unwrap();
        e.project.sequences[0].tracks[v].clips.push(c);
        e.paint_stroke(id, 0, vec![[100.0, 100.0], [120.0, 100.0]], Some([40.0, 90.0]));
        e.paint_stroke(id, 0, vec![[200.0, 50.0]], None);
        let fx = &e.seq().find_clip(id).unwrap().1.effects[0];
        let Some(EffectData::Paint(s)) = &fx.data else { panic!("{}", e.status) };
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].offset, [-60.0, -10.0]);
        assert_eq!(s[1].offset, [-60.0, -10.0], "the offset stays for later strokes");
        assert_eq!((s[0].tool, s[0].lasts, s[0].radius), (Tool::Clone, Lasts::Always, 10.0));
        e.undo();
        let Some(EffectData::Paint(s)) = &e.seq().find_clip(id).unwrap().1.effects[0].data else { panic!() };
        assert_eq!(s.len(), 1, "undo takes the last stroke");
        e.paint_clear(id, 0);
        assert!(e.seq().find_clip(id).unwrap().1.effects[0].data.is_none());
    }
}
