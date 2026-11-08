//! Time remapping (see `reelforge_core::speed::set_remap`): turning it on and
//! off, keys at the playhead, easing, and how retimed footage fills the
//! moments between its frames.

use super::Editor;
use reelforge_core::anim::{Curve, Interp};
use reelforge_core::{Clip, ClipId, FrameBlend, Time};

impl Editor {
    /// Change clip `id`'s remap with `f` (given its present curve, or `None`
    /// for "make one from its speed"), as one undo step.
    fn change_remap(&mut self, id: ClipId, what: &str, f: impl FnOnce(&Clip, Option<Curve<f32>>) -> Option<Curve<f32>>, blend: Option<FrameBlend>) {
        self.commit_live_edit();
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let c = c.clone();
        let remap = f(&c, c.remap.clone());
        match reelforge_core::speed::set_remap(&self.project, self.active, id, remap, blend.unwrap_or(c.frame_blend)) {
            Ok(cmd) => {
                self.execute(cmd);
                self.status = what.into();
            }
            Err(e) => self.status = format!("Can't remap the time: {e}"),
        }
    }

    /// The playhead as clip `c`'s own time (kept inside the clip).
    fn clip_time_here(&self, c: &Clip) -> Time {
        let t = self.playback.playhead.max(c.timeline_start).min(c.timeline_end() - Time(1));
        c.source_time(t)
    }

    /// Time remapping on (keys at the clip's start and end showing what it
    /// shows now) or off (back to its speed).
    pub fn time_remap(&mut self, id: ClipId, on: bool) {
        let msg = if on { "Time remapping: set which moment of the footage shows at the playhead, and a key goes there" } else { "Time remapping off: the clip plays at its speed again" };
        self.change_remap(id, msg, |c, _| on.then(|| reelforge_core::speed::remap_from_speed(c)), None);
    }

    /// Show media time `secs` at the playhead (a key there, made or moved).
    pub fn time_remap_value(&mut self, id: ClipId, secs: f32) {
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let ct = self.clip_time_here(c);
        self.change_remap(id, "Time remap key set", |c, r| {
            let mut r = r.unwrap_or_else(|| reelforge_core::speed::remap_from_speed(c));
            r.set(ct, secs.max(0.0));
            Some(r)
        }, None);
    }

    /// A key at the playhead: added (holding what shows there now), or
    /// removed if there is one (the first two stay).
    pub fn time_remap_key(&mut self, id: ClipId) {
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let ct = self.clip_time_here(c);
        self.change_remap(id, "Time remap key", |c, r| {
            let mut r = r.unwrap_or_else(|| reelforge_core::speed::remap_from_speed(c));
            if r.index_at(ct).is_some() {
                if r.0.len() > 2 {
                    r.remove_at(ct);
                }
            } else {
                let here = c.media_time(ct).as_secs_f64() as f32;
                r.set(ct, here);
            }
            Some(r)
        }, None);
    }

    /// Key `index`'s media time (dragged in the Speed section's graph).
    pub fn time_remap_move_key(&mut self, id: ClipId, index: usize, secs: f32) {
        self.change_remap(id, "Time remap key moved", |_, r| {
            let mut r = r?;
            if let Some(k) = r.0.get_mut(index) {
                k.value = secs.max(0.0);
            }
            Some(r)
        }, None);
    }

    /// Speed changes between keys: gradual (eased) or sudden (linear), or held frames.
    pub fn time_remap_interp(&mut self, id: ClipId, interp: Interp) {
        let msg = match interp {
            Interp::Ease => "Time remap: speed changes ease in and out",
            Interp::Hold => "Time remap: each key's frame holds until the next",
            _ => "Time remap: steady speed between keys",
        };
        self.change_remap(id, msg, |_, r| {
            let mut r = r?;
            for k in &mut r.0 {
                (k.interp, k.ease_in, k.ease_out) = (interp, None, None);
            }
            Some(r)
        }, None);
    }

    /// How carefully optical flow works on clip `id` (and the clips linked to it).
    pub fn set_flow_quality(&mut self, id: ClipId, quality: reelforge_core::FlowQuality) {
        self.commit_live_edit();
        let clips: Vec<(ClipId, reelforge_core::FlowQuality)> = self.seq().link_group(id).iter().map(|(_, c)| (c.id, quality)).collect();
        if clips.is_empty() {
            return;
        }
        self.execute(Box::new(reelforge_core::speed::SetFlowQuality { seq: self.active, clips }));
        self.status = format!("Optical flow quality: {quality:?}");
    }

    /// What retimed footage shows between its frames.
    pub fn set_frame_blend(&mut self, id: ClipId, blend: FrameBlend) {
        let msg = match blend {
            FrameBlend::Nearest => "Between frames: the nearest frame",
            FrameBlend::Blend => "Between frames: frame blending (the two either side cross-faded)",
            FrameBlend::Flow => "Between frames: optical flow (the frames either side moved along the motion and cross-faded)",
        };
        self.change_remap(id, msg, |_, r| r, Some(blend));
    }
}
