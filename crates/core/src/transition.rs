//! Video transitions between two clips that touch on a track. How long a
//! transition lasts is the incoming clip's `dissolve_in`; which kind it is is
//! its `transition`.

use crate::project::{ClipId, Project};
use crate::undo::Command;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Transition {
    /// The outgoing picture fades into the incoming one.
    #[default]
    Dissolve,
    /// Fade out to black, then in from black.
    DipToBlack,
    /// Fade out to white, then in from white.
    DipToWhite,
    /// A soft edge sweeps across, revealing the incoming clip. The direction is the way the edge moves.
    WipeLeft,
    WipeRight,
    WipeUp,
    WipeDown,
    /// A circle opens from the middle.
    Iris,
    /// The incoming clip slides in over the outgoing one, moving this way.
    SlideLeft,
    SlideRight,
    SlideUp,
    SlideDown,
    /// The incoming clip pushes the outgoing one out of the frame, moving this way.
    PushLeft,
    PushRight,
    PushUp,
    PushDown,
}

impl Transition {
    pub const ALL: [Transition; 16] = [
        Transition::Dissolve,
        Transition::DipToBlack,
        Transition::DipToWhite,
        Transition::WipeLeft,
        Transition::WipeRight,
        Transition::WipeUp,
        Transition::WipeDown,
        Transition::Iris,
        Transition::SlideLeft,
        Transition::SlideRight,
        Transition::SlideUp,
        Transition::SlideDown,
        Transition::PushLeft,
        Transition::PushRight,
        Transition::PushUp,
        Transition::PushDown,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Transition::Dissolve => "Cross-dissolve",
            Transition::DipToBlack => "Dip to black",
            Transition::DipToWhite => "Dip to white",
            Transition::WipeLeft => "Wipe left",
            Transition::WipeRight => "Wipe right",
            Transition::WipeUp => "Wipe up",
            Transition::WipeDown => "Wipe down",
            Transition::Iris => "Iris (circle)",
            Transition::SlideLeft => "Slide left",
            Transition::SlideRight => "Slide right",
            Transition::SlideUp => "Slide up",
            Transition::SlideDown => "Slide down",
            Transition::PushLeft => "Push left",
            Transition::PushRight => "Push right",
            Transition::PushUp => "Push up",
            Transition::PushDown => "Push down",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Transition::Dissolve => "The classic: one shot melts into the next.",
            Transition::DipToBlack => "Fade out to black and back in: a pause, or the passing of time.",
            Transition::DipToWhite => "Fade through white: a flash, a memory, a dream.",
            Transition::WipeLeft | Transition::WipeRight | Transition::WipeUp | Transition::WipeDown => "A soft edge sweeps across the frame.",
            Transition::Iris => "A circle opens from the middle of the frame.",
            Transition::SlideLeft | Transition::SlideRight | Transition::SlideUp | Transition::SlideDown => "The next shot slides in over this one.",
            Transition::PushLeft | Transition::PushRight | Transition::PushUp | Transition::PushDown => "The next shot pushes this one out of the frame.",
        }
    }

    /// The direction something moves, in frame units (x right, y down).
    pub fn direction(self) -> [f32; 2] {
        match self {
            Transition::WipeLeft | Transition::SlideLeft | Transition::PushLeft => [-1.0, 0.0],
            Transition::WipeRight | Transition::SlideRight | Transition::PushRight => [1.0, 0.0],
            Transition::WipeUp | Transition::SlideUp | Transition::PushUp => [0.0, -1.0],
            Transition::WipeDown | Transition::SlideDown | Transition::PushDown => [0.0, 1.0],
            _ => [0.0, 0.0],
        }
    }

    pub fn is_wipe(self) -> bool {
        matches!(self, Transition::WipeLeft | Transition::WipeRight | Transition::WipeUp | Transition::WipeDown | Transition::Iris)
    }
    pub fn is_slide(self) -> bool {
        matches!(self, Transition::SlideLeft | Transition::SlideRight | Transition::SlideUp | Transition::SlideDown)
    }
    pub fn is_push(self) -> bool {
        matches!(self, Transition::PushLeft | Transition::PushRight | Transition::PushUp | Transition::PushDown)
    }
    /// The colour a dip goes through (linear RGB).
    pub fn dip_color(self) -> Option<[f32; 3]> {
        match self {
            Transition::DipToBlack => Some([0.0; 3]),
            Transition::DipToWhite => Some([1.0; 3]),
            _ => None,
        }
    }

    /// Where the incoming (`to`) and outgoing (`from`) layers are moved, in
    /// frame pixels, `mix` of the way through a slide or push.
    pub fn offsets(self, mix: f32, frame: [f32; 2]) -> ([f32; 2], [f32; 2]) {
        let [dx, dy] = self.direction();
        // Ease in and out, so the motion starts and lands gently.
        let m = mix.clamp(0.0, 1.0);
        let e = m * m * (3.0 - 2.0 * m);
        let to = [-dx * frame[0] * (1.0 - e), -dy * frame[1] * (1.0 - e)];
        let from = if self.is_push() { [dx * frame[0] * e, dy * frame[1] * e] } else { [0.0, 0.0] };
        (to, from)
    }
}

/// Change the kind of transition into a clip.
pub struct SetTransition {
    pub seq: usize,
    pub id: ClipId,
    pub old: Transition,
    pub new: Transition,
}

impl Command for SetTransition {
    fn name(&self) -> &str {
        "Change transition"
    }
    fn apply(&mut self, p: &mut Project) {
        if let Some((_, c)) = p.sequences[self.seq].find_clip_mut(self.id) {
            c.transition = self.new;
        }
    }
    fn undo(&mut self, p: &mut Project) {
        if let Some((_, c)) = p.sequences[self.seq].find_clip_mut(self.id) {
            c.transition = self.old;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slides_and_pushes_move_the_right_way() {
        let frame = [1920.0, 1080.0];
        // Slide left: the new shot starts off to the right and lands in place.
        assert_eq!(Transition::SlideLeft.offsets(0.0, frame), ([1920.0, 0.0], [0.0, 0.0]));
        assert_eq!(Transition::SlideLeft.offsets(1.0, frame), ([0.0, 0.0], [0.0, 0.0]));
        // Push up: half way, the new one is half a frame below, the old one half a frame above.
        let (to, from) = Transition::PushUp.offsets(0.5, frame);
        assert_eq!((to, from), ([0.0, 540.0], [0.0, -540.0]));
        assert_eq!(Transition::DipToWhite.dip_color(), Some([1.0; 3]));
        assert!(Transition::Iris.is_wipe() && !Transition::Dissolve.is_wipe());
    }
}
