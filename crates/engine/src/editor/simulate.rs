//! Ready-made particles & simulation layers: fire, smoke, an explosion,
//! rain, snow, fog, a muzzle flash, a bullet hit, a gun shot, a fluid, and a
//! shatter or atomic particles on the selected layer. Each is ordinary
//! effects (particles, shatter, muzzle flash, bullet hit, fluid) set up and
//! timed, so everything can be changed afterwards in the Controls panel.

use super::Editor;
use reelforge_core::effects::{bullet, fluid_params as fl, liquid as lq, muzzle, ocean as oc, shatter, water as ws};
use reelforge_core::particles as p;
use reelforge_core::undo::{AddClip, Batch, Command};
use reelforge_core::{Clip, ClipId, Effect, EffectKind, MediaId, TitleSpec, Time};

/// What can be added (the menu's order): (name, label).
pub const SIMULATIONS: [(&str, &str); 26] = [
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
    ("ocean", "Ocean"),
    ("ocean_water", "Ocean, water only (for a sky of your own)"),
    ("storm", "Stormy sea at sunset"),
    ("rain_water", "Rain on water"),
    ("ripples", "Ripples from drops at a point"),
    ("waves", "Waves rolling in"),
    ("pour", "Pour water"),
    ("waterfall", "Waterfall"),
    ("fill", "Fill with water"),
    ("tank", "Fill a tank (with depth)"),
    ("water_round", "Water round the selected layer (green screen)"),
    ("pour_round", "Pour water onto the selected layer (green screen)"),
    ("sea_round", "Sea round the selected layer (green screen)"),
];

pub const ON_SELECTED: [&str; 6] = ["shatter", "atomic", "disintegrate", "water_round", "pour_round", "sea_round"];

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

/// A liquid with its sizes and speeds scaled by `k` (made for a 1080-line frame), with `set` on top.
fn liquid(k: f32, set: &[(usize, f32)]) -> Effect {
    let mut e = Effect::new(EffectKind::Liquid);
    for i in [lq::SPEED, lq::WIDTH, lq::GRAVITY] {
        e.params[i].value *= k;
    }
    // (finer than the effect's own default: these are meant to look good at once)
    e.params[lq::DETAIL].value = 1.0;
    for &(i, x) in set {
        e.params[i].value = x;
    }
    e
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
            "ocean" => self.add_effects_layer(vec![with(EffectKind::Ocean, &[])], false, "Add ocean", "Added an ocean out to the horizon: Wind speed and Choppiness make it rougher, the Camera settings move your view, the Sun lights it"),
            "ocean_water" => self.add_effects_layer(vec![with(EffectKind::Ocean, &[(oc::SHOW, 1.0)])], false, "Add ocean", "Added the sea alone, the sky left clear: put a sky of your own on a layer below"),
            "storm" => self.add_effects_layer(
                vec![with(EffectKind::Ocean, &[(oc::WIND, 18.0), (oc::CHOP, 115.0), (oc::FOAM, 140.0), (oc::SWELL, 120.0), (oc::SUN_HEIGHT, 3.0), (oc::SUN_DIR, 25.0), (oc::HAZE, 55.0), (oc::SKY, 0x55607a as f32), (oc::HORIZON, 0xe0a070 as f32), (oc::CAM_HEIGHT, 7.0)])],
                false,
                "Add stormy sea",
                "Added a stormy sea at sunset: high, choppy waves with foam, the sun low on the horizon",
            ),
            "rain_water" => self.add_effects_layer(vec![with(EffectKind::WaterSurface, &[(ws::RAIN, 20.0), (ws::DROP_SIZE, 14.0 * k), (ws::LAYER, 0.0)])], false, "Add rain on water", "Added rain falling on water: put it over a picture of the bottom (it shows through, bent by the ripples) or keep its sandy floor; Rain sets how heavy"),
            "ripples" => self.add_effects_layer(vec![with(EffectKind::WaterSurface, &[(ws::RAIN, 0.0), (ws::DROP_EVERY, 1.2), (ws::DROP_SIZE, 30.0 * k)])], false, "Add ripples", "Added drops falling at one point (drag it in the Viewer), rings spreading out from each and bouncing off the frame's edges"),
            "waves" => self.add_effects_layer(vec![with(EffectKind::WaterSurface, &[(ws::RAIN, 0.0), (ws::WAVE_HEIGHT, 100.0), (ws::WAVELENGTH, 160.0 * k), (ws::WAVE_DIR, -70.0)])], false, "Add waves", "Added waves rolling in from one side: Wavelength and Wave direction set them; put a green-screen object on the layer and they bounce off it"),
            "tank" => self.add_effects_layer(vec![liquid(k, &[(lq::FILL, 20.0), (lq::DEPTH, 450.0 * k), (lq::VIEW, 22.0)])], false, "Add a tank of water", "Added water pouring into a tank, seen from a little above (Depth makes it a body of water; View from above sets the angle). Make the layer 3D (Transform) and it's seen by the scene's camera instead, in perspective as the camera or the layer moves"),
            "pour" => self.add_effects_layer(vec![liquid(k, &[])], false, "Add pouring water", "Added water pouring in and splashing about the frame's floor (drag the source in the Viewer). It's worked out from the layer's start, so the first play takes a moment"),
            "waterfall" => self.add_effects_layer(
                vec![liquid(k, &[(lq::X, 8.0), (lq::Y, 18.0), (lq::DIRECTION, 0.0), (lq::SPEED, 220.0 * k), (lq::WIDTH, 90.0 * k), (lq::FLOW, 180.0), (lq::EDGES, 1.0), (lq::FILL, 12.0)])],
                false,
                "Add waterfall",
                "Added a waterfall: water spilling over from the left into a pool, flowing out at the sides",
            ),
            "fill" => self.add_effects_layer(vec![liquid(k, &[(lq::X, 50.0), (lq::Y, 0.0), (lq::DIRECTION, -90.0), (lq::SPEED, 300.0 * k), (lq::WIDTH, 80.0 * k), (lq::FLOW, 250.0), (lq::FILL, 15.0)])], false, "Add filling water", "Added water pouring in from above and filling the frame up"),
            "shatter" | "atomic" | "disintegrate" | "water_round" | "pour_round" | "sea_round" => self.add_to_selected(what),
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
        let Some((track, c)) = self.seq().find_clip(id) else { return };
        if self.seq().tracks.get(track).is_none_or(|t| t.kind != reelforge_core::TrackKind::Video) || c.scene.is_some() || c.adjustment {
            self.status = "Select a picture layer (footage, a still, a title or a composition)".into();
            return;
        }
        let t = self.playback.playhead.max(c.timeline_start).min(c.timeline_end());
        let src = c.source_time(t).as_secs_f64() as f32;
        let k = self.seq().height as f32 / 1080.0;
        let (fx, msg) = if what == "water_round" {
            (
                with(EffectKind::WaterSurface, &[(ws::RAIN, 4.0), (ws::DROP_EVERY, 1.5), (ws::DROP_SIZE, 30.0 * k), (ws::SOLIDS, 2.0), (ws::LAYER, 1.0)]),
                "The layer is in water now: what's in front of its green screen is solid, ripples bounce off it and it pushes the water as it moves. Solid objects sets what's solid (its own transparency, a blue screen…); The layer: Solid objects hidden leaves only the water, to export with transparency",
            )
        } else if what == "sea_round" {
            (
                with(EffectKind::Ocean, &[(oc::SOLIDS, 2.0), (oc::SHOW, 3.0)]),
                "The layer is at sea now: what's in front of its green screen sits in the water, with white water where the waves meet it, spray where they hit, and a wake and foam behind it as it moves. Place it on the water's surface in the Viewer; Solid objects sets what's solid (its own transparency, a blue screen…)",
            )
        } else if what == "pour_round" {
            (
                liquid(k, &[(lq::X, 45.0), (lq::Y, 0.0), (lq::DIRECTION, -90.0), (lq::SPEED, 120.0 * k), (lq::WIDTH, 70.0 * k), (lq::SOLIDS, 2.0), (lq::SHOW, 1.0)]),
                "Water pours onto the layer now: what's in front of its green screen is solid and the water splashes off it, the green keyed out. Show: Water only leaves just the water, to export with transparency",
            )
        } else if what == "shatter" {
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
        for (what, _) in SIMULATIONS.iter().filter(|(w, _)| !ON_SELECTED.contains(w)) {
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
        // Water round it, and water poured onto it, keyed off a green screen.
        e.add_simulation("water_round");
        e.add_simulation("pour_round");
        let (_, c) = e.seq().find_clip(e.selection.unwrap()).unwrap();
        assert_eq!(c.effects.iter().map(|f| f.kind).collect::<Vec<_>>(), [EffectKind::WaterSurface, EffectKind::Liquid]);
        assert_eq!(c.effects[1].params[lq::SOLIDS].value, 2.0);
        e.undo();
        e.undo();
        assert!(e.seq().find_clip(e.selection.unwrap()).unwrap().1.effects.is_empty());
    }
}

/// What a water element export renders: a copy of the project with only the
/// clip (its sequence's cameras and lights stay too), its water effects set
/// to give the water alone, and the clip's span on the timeline.
pub struct WaterElement {
    pub project: reelforge_core::Project,
    pub seq: usize,
    pub start: Time,
    pub end: Time,
    /// A file name for it ("<clip> water").
    pub name: String,
}

impl WaterElement {
    /// How it's exported to `path`: ProRes 4444 with transparency, at the
    /// sequence's size, over the clip's span, no sound.
    pub fn settings(&self, path: std::path::PathBuf) -> reelforge_export::ExportSettings {
        use reelforge_media::{ColorOut, Fit, Quality, VideoCodec};
        let (w, h) = self.project.sequences.get(self.seq).map_or((1920, 1080), |s| (s.width, s.height));
        reelforge_export::ExportSettings {
            path,
            width: w,
            height: h,
            quality: Quality::High,
            include_audio: false,
            start: self.start,
            end: self.end,
            video: Some(VideoCodec::ProRes4444),
            hardware: false,
            fit: Fit::Letterbox,
            sequence: self.seq,
            color: ColorOut::Sdr8,
            bitrate_kbps: None,
            alpha: true,
            frame_rate: None,
            between_frames: None,
        }
    }
}

impl Editor {
    /// The water element of clip `id` (Timeline → Water → Export water
    /// element): see [`WaterElement`]. The project itself isn't changed.
    pub fn water_element(&self, id: ClipId) -> Result<WaterElement, String> {
        let seq = self.active;
        let s = self.project.sequences.get(seq).ok_or("No sequence")?;
        let (track, clip) = s.find_clip(id).ok_or("Select the clip with the water first")?;
        let water = |e: &Effect| e.enabled && matches!(e.kind, EffectKind::Ocean | EffectKind::WaterSurface | EffectKind::Liquid);
        if !clip.effects.iter().any(water) {
            return Err("This clip has no water effect (ocean, water surface or pouring water) to export".into());
        }
        let (start, end) = (clip.timeline_start.max(Time::ZERO), clip.timeline_end());
        if end <= start {
            return Err("The clip is too short to export".into());
        }
        let name = format!("{} water", clip.display_name(&self.project));
        let mut project = self.project.clone();
        let out = project.sequences.get_mut(seq).ok_or("No sequence")?;
        for (ti, t) in out.tracks.iter_mut().enumerate() {
            t.clips.retain(|c| c.id == id || c.scene.is_some());
            if ti == track {
                t.muted = false;
            }
        }
        let (_, c) = out.find_clip_mut(id).ok_or("Select the clip with the water first")?;
        for e in c.effects.iter_mut().filter(|e| water(e)) {
            let (i, v) = match e.kind {
                EffectKind::Ocean => (oc::SHOW, 1.0),
                EffectKind::WaterSurface => (ws::OUTPUT, 1.0),
                _ => (lq::SHOW, 2.0),
            };
            if let Some(p) = e.params.get_mut(i) {
                p.value = v;
                p.keys = Default::default();
            }
        }
        Ok(WaterElement { project, seq, start, end, name })
    }
}

#[cfg(test)]
mod water_element_tests {
    use super::*;

    #[test]
    fn a_water_element_is_the_clip_alone_with_only_its_water() {
        let mut e = Editor::new(None, None);
        e.add_title();
        let title = e.selection.unwrap();
        e.add_simulation("pour");
        let pour = e.selection.unwrap();
        assert!(e.water_element(title).is_err(), "a title has no water");
        let w = e.water_element(pour).unwrap();
        let s = &w.project.sequences[w.seq];
        let clips: Vec<_> = s.tracks.iter().flat_map(|t| &t.clips).collect();
        assert_eq!(clips.len(), 1, "only the water's clip");
        let c = clips[0];
        assert_eq!(c.id, pour);
        assert_eq!((w.start, w.end), (c.timeline_start, c.timeline_end()));
        let fx = c.effects.iter().find(|e| e.kind == EffectKind::Liquid).unwrap();
        assert_eq!(fx.params[lq::SHOW].value, 2.0, "water only (transparent)");
        assert!(w.name.ends_with(" water"), "{}", w.name);
        // The project itself is untouched.
        assert_eq!(e.project.sequences[e.active].tracks.iter().map(|t| t.clips.len()).sum::<usize>(), 2);
    }

    /// Exported: ProRes 4444 with transparency, the background left out, the
    /// water there and the rest of the frame clear.
    #[test]
    fn a_water_element_exports_as_water_alone_with_transparency() {
        let Some((device, queue)) = reelforge_render::headless_device() else { return };
        let mut e = Editor::new(None, None);
        e.add_plane([40, 90, 200, 255]); // a solid plane under it all
        e.add_simulation("pour");
        let pour = e.selection.unwrap();
        let mut w = e.water_element(pour).unwrap();
        // (just a moment of it, once the water's in)
        w.start = Time::from_secs_f64(1.6);
        w.end = Time::from_secs_f64(1.72);
        let out = std::env::temp_dir().join(format!("rf_water_element_{}.mov", std::process::id()));
        let st = w.settings(out.clone());
        reelforge_export::export_sequence(&w.project, &st, &device, &queue, |_| true).unwrap();
        let mut dec = reelforge_media::FrameSource::open(&out, None).unwrap();
        let f = dec.frame_at_blocking(Time::ZERO, std::time::Duration::from_secs(5)).unwrap();
        let _ = std::fs::remove_file(&out);
        let alphas: Vec<u8> = f.rgba().chunks(4).map(|p| p[3]).collect();
        let clear = alphas.iter().filter(|a| **a <= 2).count();
        let water = alphas.iter().filter(|a| **a >= 60).count();
        assert!(clear > alphas.len() / 3, "the plane left out, the frame mostly clear: {clear} of {}", alphas.len());
        assert!(water > 2000, "water you can see: {water}");
    }
}
