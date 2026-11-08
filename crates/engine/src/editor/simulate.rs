//! Ready-made particles & simulation layers: fire, smoke, an explosion,
//! rain, snow, fog, a muzzle flash, a bullet hit, a gun shot, a fluid, and a
//! shatter or atomic particles on the selected layer. Each is ordinary
//! effects (particles, shatter, muzzle flash, bullet hit, fluid) set up and
//! timed, so everything can be changed afterwards in the Controls panel.

use super::Editor;
use reelforge_core::effects::{bullet, fluid_params as fl, muzzle, shatter};
use reelforge_core::particles as p;
use reelforge_core::undo::{AddClip, Batch, Command};
use reelforge_core::{Clip, ClipId, Effect, EffectKind, MediaId, TitleSpec, Time};

/// What can be added (the menu's order): (name, label).
pub const SIMULATIONS: [(&str, &str); 13] = [
    ("fire", "Fire"),
    ("smoke", "Smoke"),
    ("explosion", "Explosion"),
    ("rain", "Rain"),
    ("snow", "Snow"),
    ("fog", "Fog"),
    ("muzzle", "Muzzle flash"),
    ("bullet", "Bullet hit"),
    ("gunshot", "Gun shot (flash and hit)"),
    ("fluid", "Fluid (ink or coloured smoke)"),
    ("shatter", "Shatter the selected layer"),
    ("atomic", "Atomic particles from the selected layer"),
    ("disintegrate", "Disintegrate the selected layer"),
];

/// Particles from preset `name`, its sizes and speeds scaled by `k` (the
/// presets are made for a 1080-line frame), with `set` on top.
pub fn preset(name: &str, k: f32, set: &[(usize, f32)]) -> Effect {
    let mut e = Effect::new(EffectKind::Particles);
    if let Some((_, v)) = p::PRESETS.iter().find(|(n, _)| *n == name) {
        for (i, x) in v.iter().enumerate() {
            e.params[p::SHAPE + i].value = *x;
        }
    }
    for i in [p::EMIT_SIZE, p::SPEED, p::GRAVITY, p::WIND, p::TURBULENCE, p::SIZE_START, p::SIZE_END] {
        e.params[i].value *= k;
    }
    for &(i, x) in set {
        e.params[i].value = x;
    }
    e
}

/// A burst: the birth rate `rate` from `from` for `len` seconds, nothing otherwise.
fn burst(e: &mut Effect, from: f64, len: f64, rate: f32) {
    let s = Time::from_secs_f64;
    let keys = &mut e.params[p::RATE].keys;
    if from > 0.0 {
        keys.set(s(from - 0.01), 0.0);
    }
    keys.set(s(from), rate);
    keys.set(s(from + len), rate);
    keys.set(s(from + len + 0.01), 0.0);
}

fn with(kind: EffectKind, set: &[(usize, f32)]) -> Effect {
    let mut e = Effect::new(kind);
    for &(i, x) in set {
        e.params[i].value = x;
    }
    e
}

impl Editor {
    /// Add ready-made simulation `what` (one of [`SIMULATIONS`]).
    pub fn add_simulation(&mut self, what: &str) {
        let k = self.seq().height as f32 / 1080.0;
        let pos = |x: f32, y: f32| [(p::EMIT_X, x), (p::EMIT_Y, y)];
        match what {
            "fire" => {
                let fire = preset("Fire", k, &pos(50.0, 85.0));
                let embers = preset("Embers", k, &[(p::EMIT_X, 50.0), (p::EMIT_Y, 82.0), (p::EMIT_SIZE, 160.0 * k), (p::RATE, 12.0)]);
                let smoke = preset("Smoke", k, &[(p::EMIT_X, 50.0), (p::EMIT_Y, 70.0), (p::RATE, 10.0), (p::OPACITY, 25.0)]);
                self.add_effects_layer(vec![smoke, fire, embers], false, "Add fire", "Added fire: drag the emitters in the Viewer, and change the flames, embers and smoke in the Controls panel");
            }
            "smoke" => self.add_effects_layer(vec![preset("Smoke", k, &pos(50.0, 85.0))], false, "Add smoke", "Added smoke: drag the emitter in the Viewer; Wind and Turbulence push it about"),
            "explosion" => {
                let at = pos(50.0, 55.0);
                let mut smoke = preset("Smoke", k, &[at[0], at[1], (p::SPREAD, 360.0), (p::SPEED, 160.0 * k), (p::EMIT_SIZE, 160.0 * k), (p::LIFETIME, 4.5), (p::SIZE_START, 80.0 * k), (p::SIZE_END, 480.0 * k), (p::COLOR_START, 0x5a5450 as f32), (p::COLOR_END, 0x2a2826 as f32), (p::OPACITY, 60.0), (p::GRAVITY, -60.0 * k)]);
                burst(&mut smoke, 0.08, 0.3, 500.0);
                let mut fireball = preset("Fire", k, &[at[0], at[1], (p::SHAPE, p::SHAPE_DISC as f32), (p::EMIT_SIZE, 90.0 * k), (p::SPREAD, 360.0), (p::SPEED, 480.0 * k), (p::LIFETIME, 0.9), (p::SIZE_START, 170.0 * k), (p::SIZE_END, 70.0 * k), (p::GRAVITY, -260.0 * k), (p::COLOR_START, 0xffb050 as f32), (p::COLOR_END, 0xa01800 as f32), (p::OPACITY, 75.0)]);
                burst(&mut fireball, 0.0, 0.14, 2200.0);
                let mut flash = preset("Magic", k, &[at[0], at[1], (p::LOOK, 0.0), (p::SPEED, 40.0 * k), (p::SPREAD, 360.0), (p::LIFETIME, 0.25), (p::SIZE_START, 700.0 * k), (p::SIZE_END, 250.0 * k), (p::COLOR_START, 0xfff2c8 as f32), (p::COLOR_END, 0xff9030 as f32), (p::OPACITY, 100.0), (p::GRAVITY, 0.0), (p::TURBULENCE, 0.0)]);
                burst(&mut flash, 0.0, 0.04, 300.0);
                let mut debris = preset("Debris", k, &[at[0], at[1], (p::SPREAD, 300.0), (p::SPEED, 950.0 * k), (p::LIFETIME, 2.5)]);
                burst(&mut debris, 0.0, 0.08, 900.0);
                let mut sparks = preset("Sparks", k, &[at[0], at[1], (p::SPREAD, 360.0), (p::SPEED, 1300.0 * k), (p::LIFETIME, 1.3)]);
                burst(&mut sparks, 0.0, 0.08, 4500.0);
                self.add_effects_layer(vec![smoke, fireball, flash, debris, sparks], false, "Add explosion", "Added an explosion: a flash, a fireball, debris, sparks and smoke, set off at the layer's start. Move it by dragging its emitters, or the whole layer");
            }
            "rain" => {
                let w = self.seq().width as f32;
                let e = preset("Rain", k, &[(p::EMIT_X, 50.0), (p::EMIT_Y, -8.0), (p::EMIT_SIZE, w * 1.6)]);
                self.add_effects_layer(vec![e], false, "Add rain", "Added rain over the frame: Wind slants it, Birth rate makes it heavier");
            }
            "snow" => {
                let w = self.seq().width as f32;
                let e = preset("Snow", k, &[(p::EMIT_X, 50.0), (p::EMIT_Y, -5.0), (p::EMIT_SIZE, w * 1.4)]);
                self.add_effects_layer(vec![e], false, "Add snow", "Added snow over the frame: Wind drifts it, Birth rate makes it heavier");
            }
            "fog" => self.add_effects_layer(vec![preset("Fog", k, &[])], false, "Add fog", "Added drifting fog over the frame: Opacity thickens it, Wind moves it along"),
            "muzzle" => self.add_effects_layer(vec![with(EffectKind::MuzzleFlash, &[(muzzle::FIRST, 0.1)])], true, "Add muzzle flash", "Added a muzzle flash (an adjustment layer, so it lights the footage): drag it to the barrel in the Viewer and set its direction; Shots per second makes a burst"),
            "bullet" => self.add_effects_layer(vec![with(EffectKind::BulletHit, &[(bullet::AT, 0.1)])], true, "Add bullet hit", "Added a bullet hit (an adjustment layer over the footage): drag it where the bullet strikes and pick the surface"),
            "gunshot" => self.add_effects_layer(
                vec![with(EffectKind::MuzzleFlash, &[(muzzle::FIRST, 0.1)]), with(EffectKind::BulletHit, &[(bullet::AT, 0.15)])],
                true,
                "Add gun shot",
                "Added a gun shot: a muzzle flash, and its bullet hitting a wall a moment later. Drag both into place in the Viewer",
            ),
            "fluid" => self.add_effects_layer(vec![with(EffectKind::Fluid, &[(fl::DETAIL, 1.0)])], false, "Add fluid", "Added a fluid: coloured smoke pours from the source (drag it in the Viewer). It's worked out from the layer's start, so the first play of a long one takes a moment"),
            "shatter" | "atomic" | "disintegrate" => self.add_to_selected(what),
            _ => {}
        }
    }

    /// Shatter, or atomic particles, on the selected video layer.
    fn add_to_selected(&mut self, what: &str) {
        self.commit_live_edit();
        let Some(id) = self.selection else {
            self.status = "Select the layer first".into();
            return;
        };
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        if self.seq().tracks[self.seq().find_clip(id).unwrap().0].kind != reelforge_core::TrackKind::Video || c.scene.is_some() || c.adjustment {
            self.status = "Select a picture layer (footage, a still, a title or a composition)".into();
            return;
        }
        let t = self.playback.playhead.max(c.timeline_start).min(c.timeline_end());
        let src = c.source_time(t).as_secs_f64() as f32;
        let k = self.seq().height as f32 / 1080.0;
        let (fx, msg) = if what == "shatter" {
            (with(EffectKind::Shatter, &[(shatter::AT, src)]), "The layer shatters at the playhead: drag the impact point in the Viewer")
        } else if what == "disintegrate" {
            (with(EffectKind::Disintegrate, &[(reelforge_core::effects::disintegrate::START, src)]), "The layer breaks up into particles from the playhead, over 2 seconds: set the direction, roughness and glow in the Controls panel")
        } else {
            (preset("Atomic", k, &[]), "The layer gives off particles in its own colours: try the Disintegrate preset for only the particles")
        };
        let mut new = c.clone();
        new.effects.push(fx);
        self.execute(Box::new(reelforge_core::undo::SetClipVisual { seq: self.active, id, old: c.visual(), new: new.visual() }));
        self.status = msg.into();
    }

    /// An empty frame-sized layer with `effects` on a new top track (an
    /// adjustment layer if `adjust`, so the effects work on what's below):
    /// as long as the selected footage, or 5 seconds from the playhead.
    pub(crate) fn add_effects_layer(&mut self, effects: Vec<Effect>, adjust: bool, name: &str, status: &str) {
        self.commit_live_edit();
        let s = self.seq();
        let rate = s.frame_rate;
        let (start, dur) = match self.selection.and_then(|id| s.find_clip(id)).filter(|(_, c)| c.title.is_none()) {
            Some((_, c)) if c.media != MediaId::GENERATED || c.nested.is_some() => (c.timeline_start, c.duration),
            _ => (self.playback.playhead.snap(rate), Time::from_secs_f64(5.0).snap(rate)),
        };
        let height = s.height;
        let (add, track) = self.layer_track(start, dur);
        let id = ClipId(self.project.alloc_id());
        let mut clip = if adjust {
            Clip::new_adjustment(id, start, dur)
        } else {
            let mut spec = TitleSpec::for_frame(height);
            spec.text = String::new();
            Clip::new_title(id, start, dur, spec)
        };
        clip.effects = effects;
        let mut cmds: Vec<Box<dyn Command>> = add.into_iter().map(|a| Box::new(a) as Box<dyn Command>).collect();
        cmds.push(Box::new(AddClip { seq: self.active, track, clip }));
        self.execute(Box::new(Batch { name: name.into(), cmds }));
        self.follow_new_layer(track);
        self.select_only(Some(id));
        self.status = status.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_simulation_adds_its_effects() {
        let mut e = Editor::new(None, None);
        for (what, _) in SIMULATIONS.iter().filter(|(w, _)| !matches!(*w, "shatter" | "atomic" | "disintegrate")) {
            e.add_simulation(what);
            let id = e.selection.expect(what);
            let (_, c) = e.seq().find_clip(id).unwrap();
            assert!(!c.effects.is_empty(), "{what}");
            // Each draws something soon after its start.
            let t = c.timeline_start + Time::from_secs_f64(0.2);
            let size = [e.seq().width as f32, e.seq().height as f32];
            assert!(!c.effects_sized(t, Time::from_secs_f64(1.0 / 30.0), size).is_empty(), "{what}");
        }
        // The explosion goes off at its start and is over (bar the smoke) within a few seconds.
        e.add_simulation("explosion");
        let (_, c) = e.seq().find_clip(e.selection.unwrap()).unwrap();
        let alive = |s: f64| c.effects.iter().map(|f| reelforge_core::particles::simulate(f, Time::from_secs_f64(s)).list.len()).collect::<Vec<_>>();
        let early = alive(0.1);
        assert!(early[1] > 50 && early[4] > 100, "fireball and sparks: {early:?}");
        let later = alive(3.0);
        assert!(later[1] == 0 && later[4] == 0 && later[0] > 0, "only smoke left: {later:?}");

        // Shatter needs a selected picture layer.
        e.select_only(None);
        e.add_simulation("shatter");
        assert!(e.status.contains("Select"));
        e.add_plane([200, 0, 0, 255]);
        e.add_simulation("shatter");
        let (_, c) = e.seq().find_clip(e.selection.unwrap()).unwrap();
        assert_eq!(c.effects.last().unwrap().kind, EffectKind::Shatter);
        e.undo();
        assert!(e.seq().find_clip(e.selection.unwrap()).unwrap().1.effects.is_empty());
    }
}
