//! The viewer picture for egui: the engine's preview (decoders + GPU
//! compositor, see `reelforge_engine::preview`) drawing on egui's own GPU
//! device, with its output texture registered so panels can draw it.

use eframe::egui_wgpu::{self, wgpu};
use reelforge_core::{MediaId, Project, Time};
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use crate::render_cache::RenderCache;

pub struct Preview {
    core: reelforge_engine::preview::Preview,
    render_state: Option<egui_wgpu::RenderState>,
    texture: Option<egui::TextureId>,
    registered_generation: u64,
}

impl Deref for Preview {
    type Target = reelforge_engine::preview::Preview;
    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

impl DerefMut for Preview {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.core
    }
}

impl Preview {
    pub fn new(render_state: Option<egui_wgpu::RenderState>) -> Self {
        let gpu = render_state.as_ref().map(|rs| (rs.device.clone(), rs.queue.clone()));
        Self { core: reelforge_engine::preview::Preview::new(gpu), render_state, texture: None, registered_generation: 0 }
    }

    /// The composited frame, ready to draw with `egui::Image`/`painter.image`.
    pub fn texture(&self) -> Option<egui::TextureId> {
        self.texture
    }

    /// Render sequence `seq` at `t` (see the engine's `Preview::update`) and
    /// hand the result to egui.
    pub fn update(&mut self, project: &Project, seq: usize, t: Time, playing: bool, sources: &HashMap<MediaId, PathBuf>, cache: &mut RenderCache) {
        self.core.update(project, seq, t, playing, sources, cache);
        self.register();
    }

    /// Hand the output texture to egui (re-register after a resize).
    fn register(&mut self) {
        let (Some(rs), Some(c)) = (&self.render_state, self.core.compositor()) else { return };
        let Some(view) = c.output_view() else { return };
        let mut renderer = rs.renderer.write();
        match self.texture {
            None => {
                self.texture = Some(renderer.register_native_texture(&rs.device, view, wgpu::FilterMode::Linear));
                self.registered_generation = c.output_generation;
            }
            Some(id) if self.registered_generation != c.output_generation => {
                renderer.update_egui_texture_from_wgpu_texture(&rs.device, view, wgpu::FilterMode::Linear, id);
                self.registered_generation = c.output_generation;
            }
            Some(_) => {}
        }
    }
}
