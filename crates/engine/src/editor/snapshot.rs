//! Snapshots of the Viewer: the frame at the playhead, drawn exactly and at
//! the sequence's full size (as export would draw it, whatever the Viewer's
//! playback quality), saved as a picture.

use super::Editor;
use std::path::{Path, PathBuf};

impl Editor {
    /// Where snapshots go: the folder chosen in their menu, else a Snapshots
    /// folder beside the project, or in Pictures before the project is saved.
    pub fn snapshot_folder(&self) -> PathBuf {
        if let Some(f) = &self.prefs.snapshot_folder {
            return f.clone();
        }
        if let Some(dir) = self.project_path.as_deref().and_then(Path::parent) {
            return dir.join("Snapshots");
        }
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        home.join("Pictures").join("Reelforge snapshots")
    }

    /// Save the frame at the playhead: to `to` (PNG, or JPEG for .jpg/.jpeg),
    /// or with a name of its own (project, sequence and timecode) in
    /// [`Self::snapshot_folder`]. Adds it to the Media panel if the
    /// preference says to. Returns where it went.
    pub fn snapshot(&mut self, to: Option<PathBuf>) -> Result<PathBuf, String> {
        self.commit_live_edit();
        let (device, queue) = self.preview.gpu().ok_or("there's no graphics device to draw the frame with")?;
        let t = self.playback.playhead;
        let s = self.seq();
        let rate = s.frame_rate;
        let (rgba, [w, h]) = reelforge_export::render_still(&self.project, self.active, t, &device, &queue).map_err(|e| format!("couldn't draw the frame: {e:#}"))?;
        let path = match to {
            Some(p) => p,
            None => {
                let base = self.project_path.as_deref().and_then(Path::file_stem).map(|n| n.to_string_lossy().into_owned());
                let name = match base {
                    Some(b) if self.project.sequences.len() > 1 || b != s.name => format!("{b} - {}", s.name),
                    Some(b) => b,
                    None => s.name.clone(),
                };
                let stem = format!("{} {}", safe_name(&name), t.timecode(rate).replace([':', ';'], "-"));
                let dir = self.snapshot_folder();
                std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
                unique(&dir, &stem, "png")
            }
        };
        write_picture(&path, &rgba, w, h)?;
        let what = format!("Snapshot of {} saved: {}", t.timecode(rate), path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        if self.prefs.snapshot_import {
            self.import(std::slice::from_ref(&path));
            self.status = format!("{what} (added to the Media panel)");
        } else {
            self.status = what;
        }
        Ok(path)
    }
}

/// `name` with the characters file names can't have replaced.
fn safe_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c }).collect();
    let s = s.trim().trim_end_matches('.').to_string();
    if s.is_empty() { "Snapshot".into() } else { s }
}

/// `dir/stem.ext`, or `dir/stem (2).ext` and so on if that's taken.
fn unique(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..).map(|n| dir.join(format!("{stem} ({n}).{ext}"))).find(|p| !p.exists()).unwrap()
}

/// RGBA8 as a PNG, or as a JPEG if the path ends .jpg or .jpeg (written to a
/// temporary file first, so a failure never leaves half a picture).
fn write_picture(path: &Path, rgba: &[u8], w: u32, h: u32) -> Result<(), String> {
    let jpeg = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"));
    let mut bytes = Vec::new();
    if jpeg {
        let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        jpeg_encoder::Encoder::new(&mut bytes, 95).encode(&rgb, w as u16, h as u16, jpeg_encoder::ColorType::Rgb).map_err(|e| e.to_string())?;
    } else {
        let mut enc = png::Encoder::new(&mut bytes, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        let mut wr = enc.write_header().map_err(|e| e.to_string())?;
        let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        wr.write_image_data(&rgb).map_err(|e| e.to_string())?;
        wr.finish().map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("couldn't write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(safe_name("Cut: v2/final?"), "Cut_ v2_final_");
        assert_eq!(safe_name("  ..  "), "Snapshot");
        let dir = std::env::temp_dir().join(format!("rf-snap-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique(&dir, "a", "png"), dir.join("a.png"));
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        assert_eq!(unique(&dir, "a", "png"), dir.join("a (2).png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A title on a black sequence: the snapshot is a full-size PNG with the
    /// frame in it, named by timecode, and goes into the Media panel if asked.
    #[test]
    fn saves_the_frame_at_the_playhead() {
        let dir = std::env::temp_dir().join(format!("rf-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(Editor::new(None, None).snapshot(None).is_err(), "no graphics device: it says so");
        let Some(gpu) = reelforge_render::headless_device() else { return };
        let mut e = Editor::new(Some(gpu), None);
        e.prefs.snapshot_folder = Some(dir.clone());
        e.add_title();
        e.playback.seek(reelforge_core::Time::from_secs_f64(1.0));
        let p = e.snapshot(None).unwrap();
        assert_eq!(p.parent().unwrap(), dir);
        assert!(p.file_name().unwrap().to_string_lossy().ends_with("00-00-01-00.png"), "{}", p.display());
        let dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&p).unwrap()));
        let r = dec.read_info().unwrap();
        let s = e.seq();
        assert_eq!((r.info().width, r.info().height), (s.width, s.height));
        // Again: a name of its own.
        let p2 = e.snapshot(None).unwrap();
        assert_ne!(p, p2);
        e.prefs.snapshot_import = true;
        let n = e.project.media.len();
        let p3 = e.snapshot(Some(dir.join("x.jpg"))).unwrap();
        assert_eq!(&std::fs::read(&p3).unwrap()[..2], &[0xFF, 0xD8], "a JPEG");
        assert_eq!(e.project.media.len(), n + 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
