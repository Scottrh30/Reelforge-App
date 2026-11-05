//! Running short of disk space: look now and then at the drives the app
//! writes to, warn (with what can be cleared), and stop filling them up.

use super::Editor;
use crate::disk::{self, size_text, DiskLow};
use std::path::PathBuf;

impl Editor {
    /// The folders the app writes to by itself: rendered previews, proxies,
    /// autosaves, and the project's own folder.
    fn written_places(&self) -> Vec<PathBuf> {
        let mut v = vec![reelforge_media::proxy::cache_dir().join("proxies"), crate::safety::autosave_dir()];
        if let Some(d) = &self.render_cache.disk {
            v.push(d.dir().to_owned());
        }
        if let Some(p) = self.project_path.as_ref().and_then(|p| p.parent()) {
            v.push(p.to_owned());
        }
        v
    }

    /// Look at how much space is free where the app writes. When it's short
    /// (under `disk_low_at`), say so once with what could be cleared, keep no
    /// more rendered frames on disk and start no proxies by themselves; when
    /// there's room again, carry on as before.
    pub fn check_disk(&mut self) {
        self.disk_checked = Some(std::time::Instant::now());
        let fullest = self.written_places().into_iter().filter_map(|p| disk::free_bytes(&p).map(|f| (f, p))).min_by_key(|(f, _)| *f);
        let low = fullest.filter(|(f, _)| *f < self.disk_low_at);
        if let Some(d) = &self.render_cache.disk {
            d.set_paused(low.is_some());
        }
        match low {
            None => {
                self.disk_low = None;
                self.disk_dismissed = false;
            }
            Some((free, place)) => {
                let render_bytes = self.render_cache.disk.as_ref().map_or(0, |d| d.stats().0);
                let old = disk::unused_proxies(&self.project);
                let was = self.disk_low.is_some();
                self.disk_low = Some(DiskLow { free, place: place.display().to_string(), render_bytes, old_proxies: old.len(), old_proxy_bytes: old.iter().map(|o| o.1).sum() });
                if !was {
                    self.status = format!("Running out of disk space: {} free where Reelforge saves its files. Rendered previews are no longer kept on disk, and proxies aren't made by themselves", size_text(free));
                }
            }
        }
    }

    /// Make room: "render" clears the rendered previews (memory and disk),
    /// "proxies" deletes the proxies of media not in this project. Looks
    /// at the space again afterwards.
    pub fn free_disk_space(&mut self, what: &str) {
        let before = self.disk_low.as_ref().map(|d| d.free);
        match what {
            "render" => self.render_cache.clear_all(true),
            "proxies" => {
                let (n, bytes) = disk::delete_unused_proxies(&self.project);
                self.status = format!("Deleted {n} unused prox{} ({})", if n == 1 { "y" } else { "ies" }, size_text(bytes));
            }
            _ => return,
        }
        self.check_disk();
        let now = self.written_places().iter().filter_map(|p| disk::free_bytes(p)).min();
        if what == "render" {
            self.status = format!("Rendered previews cleared{}", now.map_or(String::new(), |f| format!(": {} free now", size_text(f))));
        }
        if self.disk_low.is_some() && before.is_some() {
            self.status += ". Still short of space: free some more on that drive";
        }
        self.touch();
    }
}
