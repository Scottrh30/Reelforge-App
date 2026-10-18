//! The plugin manager: finds plugin folders, checks and loads them, reloads
//! them when their files change (so you can edit a shader and see the result
//! straight away), and installs new ones. The apps' Plugins windows show it.
//!
//! Plugins are looked for in:
//! - the user plugin folder (where Install puts them): `%APPDATA%\Reelforge\plugins`
//!   on Windows, `~/.local/share/reelforge/plugins` on Linux,
//!   `~/Library/Application Support/Reelforge/plugins` on macOS;
//! - a `plugins` folder next to the program (plugins shipped with it);
//! - any folders listed in the `REELFORGE_PLUGINS` environment variable.

use reelforge_core::plugin::{self, PluginDef, MANIFEST};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Where a plugin was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The user plugin folder: can be removed from the Plugins window.
    User,
    /// Next to the program.
    Bundled,
    /// From REELFORGE_PLUGINS.
    Extra,
}

/// One plugin folder and how loading it went.
#[derive(Clone, Debug)]
pub struct Entry {
    pub folder: PathBuf,
    pub source: Source,
    pub result: Result<Arc<PluginDef>, String>,
}

impl Entry {
    pub fn title(&self) -> String {
        match &self.result {
            Ok(d) => d.name.clone(),
            Err(_) => self.folder.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }
}

pub struct PluginManager {
    pub open: bool,
    pub entries: Vec<Entry>,
    /// Watch plugin files and reload when they change.
    pub live_reload: bool,
    fingerprint: u64,
    last_check: Instant,
    /// A plugin folder waiting for "Remove" to be confirmed.
    pub confirm_remove: Option<PathBuf>,
    /// Extra roots (tests; normally empty).
    pub extra_roots: Vec<PathBuf>,
    /// Skip the user and program folders (tests).
    pub only_extra: bool,
}

impl Default for PluginManager {
    fn default() -> Self {
        Self { open: false, entries: Vec::new(), live_reload: true, fingerprint: 0, last_check: Instant::now(), confirm_remove: None, extra_roots: Vec::new(), only_extra: false }
    }
}

/// The user plugin folder.
pub fn user_dir() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        env("APPDATA").map(|p| p.join("Reelforge"))
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support/Reelforge"))
    } else {
        env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local/share"))).map(|p| p.join("reelforge"))
    };
    base.unwrap_or_else(|| std::env::temp_dir().join("reelforge")).join("plugins")
}

/// The `plugins` folder next to the program.
pub fn bundled_dir() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("plugins")))
}

/// Check a plugin folder completely: its manifest and that its shader compiles.
pub fn check_folder(folder: &Path) -> Result<PluginDef, String> {
    let def = plugin::load_folder(folder)?;
    reelforge_render::plugin::validate(&def)?;
    Ok(def)
}

/// The plugin folders directly inside `root` (each holding a plugin.toml).
fn plugin_folders(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join(MANIFEST).is_file() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .collect();
    v.sort();
    v
}

impl PluginManager {
    fn roots(&self) -> Vec<(PathBuf, Source)> {
        let mut v = Vec::new();
        if !self.only_extra {
            v.push((user_dir(), Source::User));
            if let Some(b) = bundled_dir() {
                v.push((b, Source::Bundled));
            }
            if let Some(list) = std::env::var_os("REELFORGE_PLUGINS") {
                v.extend(std::env::split_paths(&list).map(|p| (p, Source::Extra)));
            }
        }
        v.extend(self.extra_roots.iter().map(|p| (p.clone(), Source::Extra)));
        v
    }

    /// Something that changes when any plugin file is added, removed or edited.
    fn current_fingerprint(&self) -> u64 {
        let mut h = DefaultHasher::new();
        for (root, _) in self.roots() {
            for folder in plugin_folders(&root) {
                folder.hash(&mut h);
                if let Ok(rd) = std::fs::read_dir(&folder) {
                    let mut files: Vec<(PathBuf, u64, Option<std::time::SystemTime>)> = rd
                        .filter_map(|e| e.ok())
                        .filter_map(|e| e.metadata().ok().filter(|m| m.is_file()).map(|m| (e.path(), m.len(), m.modified().ok())))
                        .collect();
                    files.sort();
                    files.hash(&mut h);
                }
            }
        }
        h.finish()
    }

    /// Look at every plugin folder again, load the good ones and make them
    /// available. Returns a one-line summary.
    pub fn scan(&mut self) -> String {
        let old: Vec<(String, Result<u64, String>)> = self.entries.iter().map(|e| (e.folder.display().to_string(), e.result.as_ref().map(|d| d.revision).map_err(|e| e.clone()))).collect();
        let mut entries: Vec<Entry> = Vec::new();
        let mut seen: Vec<(String, PathBuf)> = Vec::new();
        for (root, source) in self.roots() {
            for folder in plugin_folders(&root) {
                let mut result = check_folder(&folder).map(Arc::new);
                if let Ok(d) = &result {
                    if let Some((_, other)) = seen.iter().find(|(id, _)| *id == d.id) {
                        result = Err(format!("another plugin with the same id (\"{}\") is already loaded from {}; give one of them a different id", d.id, other.display()));
                    } else {
                        seen.push((d.id.clone(), folder.clone()));
                    }
                }
                entries.push(Entry { folder, source, result });
            }
        }
        plugin::set_installed(entries.iter().filter_map(|e| e.result.as_ref().ok().cloned()).collect());
        self.entries = entries;
        self.fingerprint = self.current_fingerprint();
        let ok = self.entries.iter().filter(|e| e.result.is_ok()).count();
        let bad = self.entries.len() - ok;
        // Name what changed, when it's one thing (the usual case while editing a plugin).
        let changed: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| {
                let key = e.folder.display().to_string();
                match old.iter().find(|(k, _)| *k == key) {
                    None => true,
                    Some((_, Err(prev))) => e.result.as_ref().err() != Some(prev),
                    Some((_, Ok(_))) => e.result.is_err(),
                }
            })
            .collect();
        match (changed.as_slice(), bad) {
            ([one], _) => match &one.result {
                Ok(d) => format!("Plugin loaded: {}", d.name),
                Err(e) => {
                    // Where (the first line) and why (the last), on one line.
                    let (first, last) = (e.lines().next().unwrap_or(""), e.lines().last().unwrap_or(""));
                    let why = if first == last { String::new() } else { format!(" {last}") };
                    format!("Plugin {} has a problem: {}{why}", one.title(), first.trim_end_matches(':'))
                }
            },
            (_, 0) => format!("{ok} plugin{} loaded", if ok == 1 { "" } else { "s" }),
            _ => format!("{ok} plugin{} loaded, {bad} with problems (Edit → Plugins… says what)", if ok == 1 { "" } else { "s" }),
        }
    }

    /// Called every frame: with live reload on, rescans (at most once a second)
    /// when any plugin file changed. Returns a message if it did.
    pub fn poll(&mut self) -> Option<String> {
        if !self.live_reload || self.last_check.elapsed() < Duration::from_secs(1) {
            return None;
        }
        self.last_check = Instant::now();
        (self.current_fingerprint() != self.fingerprint).then(|| self.scan())
    }

    /// Copy the plugin in `src` (a folder with a plugin.toml) into the user
    /// plugin folder, replacing an installed copy of the same plugin. Checked first.
    pub fn install_folder(&mut self, src: &Path) -> Result<String, String> {
        let def = check_folder(src)?;
        let dest_root = self.install_root();
        std::fs::create_dir_all(&dest_root).map_err(|e| format!("can't make {}: {e}", dest_root.display()))?;
        // The same plugin already installed there? Replace it. Otherwise a new folder.
        let existing = plugin_folders(&dest_root).into_iter().find(|f| plugin::load_folder(f).is_ok_and(|d| d.id == def.id));
        let dest = match existing {
            Some(f) => f,
            None => free_folder(&dest_root, &folder_name(src, &def)),
        };
        if same_place(src, &dest) {
            return Ok(format!("{} is already installed", def.name));
        }
        let tmp = dest_root.join(format!(".installing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        copy_dir(src, &tmp).map_err(|e| format!("couldn't copy the plugin: {e}"))?;
        if dest.exists() {
            std::fs::remove_dir_all(&dest).map_err(|e| format!("couldn't replace the old copy: {e}"))?;
        }
        std::fs::rename(&tmp, &dest).map_err(|e| format!("couldn't install: {e}"))?;
        self.scan();
        Ok(format!("Installed {} {}", def.name, def.version).trim().to_string())
    }

    /// Install a plugin from a .zip (its plugin.toml at the top, or in one folder inside).
    pub fn install_zip(&mut self, zip_path: &Path) -> Result<String, String> {
        let root = self.install_root();
        let tmp = root.join(format!(".unzip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let result = (|| {
            let file = std::fs::File::open(zip_path).map_err(|e| format!("can't open {}: {e}", zip_path.display()))?;
            let mut z = zip::ZipArchive::new(file).map_err(|e| format!("not a zip file: {e}"))?;
            for i in 0..z.len() {
                let mut f = z.by_index(i).map_err(|e| e.to_string())?;
                // Only paths that stay inside the folder (a zip can't write elsewhere).
                let Some(rel) = f.enclosed_name() else { continue };
                let out = tmp.join(rel);
                if f.is_dir() {
                    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
                } else {
                    if let Some(d) = out.parent() {
                        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
                    }
                    let mut w = std::fs::File::create(&out).map_err(|e| e.to_string())?;
                    std::io::copy(&mut f, &mut w).map_err(|e| e.to_string())?;
                }
            }
            let folder = if tmp.join(MANIFEST).is_file() {
                tmp.clone()
            } else {
                match plugin_folders(&tmp).as_slice() {
                    [one] => one.clone(),
                    [] => return Err(format!("there's no {MANIFEST} in that zip")),
                    _ => return Err("that zip has several plugins in it: unzip it and install them one by one".into()),
                }
            };
            self.install_folder(&folder)
        })();
        let _ = std::fs::remove_dir_all(&tmp);
        result
    }

    /// Can this plugin be removed (it's in the user plugin folder)?
    pub fn removable(&self, e: &Entry) -> bool {
        e.source == Source::User || (self.only_extra && e.folder.starts_with(self.install_root()))
    }

    /// Make a new plugin from the starter template in the user folder; returns its folder.
    pub fn new_from_template(&mut self) -> Result<PathBuf, String> {
        let root = self.install_root();
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let dest = free_folder(&root, "my-effect");
        let n = dest.file_name().unwrap().to_string_lossy().into_owned();
        std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        let manifest = TEMPLATE_MANIFEST.replace("{ID}", &format!("my.{n}")).replace("{NAME}", &title_case(&n));
        std::fs::write(dest.join(MANIFEST), manifest).map_err(|e| e.to_string())?;
        std::fs::write(dest.join("effect.wgsl"), TEMPLATE_SHADER).map_err(|e| e.to_string())?;
        self.scan();
        Ok(dest)
    }

    /// Delete an installed plugin's folder (user plugins only).
    pub fn remove(&mut self, folder: &Path) -> Result<String, String> {
        let user = user_dir();
        let is_user = self.entries.iter().any(|e| e.folder == folder && (e.source == Source::User || (self.only_extra && folder.starts_with(self.install_root()))));
        if !is_user || !(folder.starts_with(&user) || folder.starts_with(self.install_root())) {
            return Err("only plugins in your plugin folder can be removed here".into());
        }
        let name = self.entries.iter().find(|e| e.folder == folder).map(Entry::title).unwrap_or_default();
        std::fs::remove_dir_all(folder).map_err(|e| format!("couldn't delete {}: {e}", folder.display()))?;
        self.scan();
        Ok(format!("Removed {name}"))
    }

    /// Where installs go: the user folder (or, in tests, the first extra root).
    pub fn install_root(&self) -> PathBuf {
        if self.only_extra {
            self.extra_roots.first().cloned().unwrap_or_else(user_dir)
        } else {
            user_dir()
        }
    }
}

fn same_place(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// A folder name for a plugin: the source folder's, unless it's a temporary one.
fn folder_name(src: &Path, def: &PluginDef) -> String {
    let n = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if n.is_empty() || n.starts_with('.') {
        def.id.replace(['/', '\\', ':'], "-")
    } else {
        n
    }
}

/// `root/name`, or `root/name-2`… if that's taken.
fn free_folder(root: &Path, name: &str) -> PathBuf {
    let mut n = 1;
    loop {
        let p = if n == 1 { root.join(name) } else { root.join(format!("{name}-{n}")) };
        if !p.exists() {
            return p;
        }
        n += 1;
    }
}

fn copy_dir(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dest.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), to)?;
        }
    }
    Ok(())
}

fn title_case(s: &str) -> String {
    s.split(['-', '_', ' ']).filter(|w| !w.is_empty()).map(|w| {
        let mut c = w.chars();
        c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
    }).collect::<Vec<_>>().join(" ")
}

const TEMPLATE_MANIFEST: &str = r#"# A Reelforge effect plugin. See PLUGINS.md (next to reelforge.exe) for everything you can do.
id = "{ID}"                # unique; projects refer to the plugin by it
name = "{NAME}"            # shown in the effects browser
version = "1.0"
author = ""
category = "Stylize"       # Colour, Blur, Keying, Stylize, Distort (or anything else: "Other plugins")
description = "A starting point: tints the picture and shifts it."
shader = "effect.wgsl"
# passes = ["effect"]      # the shader functions to run, in order (one per pass)

[[param]]                  # param(0) in the shader
name = "Amount"
default = 50
min = 0
max = 100
suffix = " %"

[[param]]                  # param(1)
name = "Shift"
default = 0
min = -200
max = 200
suffix = " px"

[[param]]                  # param_choice(2): 0, 1 or 2
name = "Tint"
type = "choice"
options = ["Warm", "Cool", "Green"]
default = 0

[[param]]                  # param_on(3): true or false
name = "Pulse"
type = "toggle"
default = 0
"#;

const TEMPLATE_SHADER: &str = r#"// Runs once for every pixel of the layer. `uv` is where: (0, 0) is the top left,
// (1, 1) the bottom right. Return the colour for that pixel (linear light,
// straight alpha). Save this file and Reelforge reloads it straight away.

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let amount = param(0) / 100.0;

    // Read the picture 'Shift' pixels to the left (so it moves right).
    let c = sample(uv - vec2(param(1), 0.0) * pixel());

    var tint = vec3(1.0, 0.85, 0.6);              // warm
    if (param_choice(2) == 1) { tint = vec3(0.6, 0.8, 1.0); }   // cool
    if (param_choice(2) == 2) { tint = vec3(0.6, 1.0, 0.6); }   // green

    var strength = amount;
    if (param_on(3)) {
        strength = amount * (0.5 + 0.5 * sin(time() * 6.0));  // pulses as the clip plays
    }
    return vec4(mix(c.rgb, c.rgb * tint, strength), c.a);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The installed-plugin list is global: tests that change it take turns.
    pub(crate) static REGISTRY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rf_plugins_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_plugin(dir: &Path, id: &str, shader: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(MANIFEST), format!("id = \"{id}\"\nname = \"{id} name\"\nversion = \"1.0\"\n[[param]]\nname = \"A\"\n")).unwrap();
        std::fs::write(dir.join("effect.wgsl"), shader).unwrap();
    }

    const GOOD: &str = "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv) * param(0); }";

    fn manager(root: &Path) -> PluginManager {
        PluginManager { extra_roots: vec![root.to_path_buf()], only_extra: true, ..Default::default() }
    }

    #[test]
    fn scans_reports_problems_and_reloads_on_change() {
        let _turn = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tmp("scan");
        write_plugin(&root.join("good"), "t.good", GOOD);
        write_plugin(&root.join("bad"), "t.bad", "fn effect(uv: vec2<f32>) -> vec4<f32> {\n  return oops;\n}");
        write_plugin(&root.join("dupe"), "t.good", GOOD);
        std::fs::create_dir_all(root.join("not-a-plugin")).unwrap();
        let mut m = manager(&root);
        let msg = m.scan();
        assert_eq!(m.entries.len(), 3);
        assert!(msg.contains("1 plugin loaded, 2 with problems"), "{msg}");
        let bad = m.entries.iter().find(|e| e.folder.ends_with("bad")).unwrap();
        assert!(bad.result.as_ref().unwrap_err().contains("effect.wgsl, line 2"), "{:?}", bad.result);
        // Two folders with the same id: the first (alphabetically) wins, the other is refused.
        let same: Vec<&Entry> = m.entries.iter().filter(|e| e.folder.ends_with("dupe") || e.folder.ends_with("good")).collect();
        assert_eq!(same.iter().filter(|e| e.result.is_ok()).count(), 1);
        assert!(same.iter().any(|e| e.result.as_ref().is_err_and(|m| m.contains("same id"))));
        assert!(plugin::find("t.good").is_some() && plugin::find("t.bad").is_none());
        // Nothing changed: no reload.
        m.last_check = Instant::now() - Duration::from_secs(2);
        assert_eq!(m.poll(), None);
        // Fix the broken one (a different size, so the change shows even within the same second).
        let rev = plugin::find("t.good").unwrap().revision;
        std::fs::write(root.join("bad/effect.wgsl"), format!("{GOOD}\n// fixed")).unwrap();
        m.last_check = Instant::now() - Duration::from_secs(2);
        let msg = m.poll().expect("reloaded");
        assert_eq!(msg, "Plugin loaded: t.bad name");
        assert!(plugin::find("t.bad").is_some());
        assert_ne!(plugin::find("t.good").unwrap().revision, rev, "reloaded plugins get a new revision");
        // Live reload off: no polling.
        m.live_reload = false;
        std::fs::write(root.join("good/effect.wgsl"), "broken").unwrap();
        m.last_check = Instant::now() - Duration::from_secs(2);
        assert_eq!(m.poll(), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_example_plugins_all_load() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        let folders = plugin_folders(&examples);
        assert_eq!(folders.len(), 5, "{folders:?}");
        for f in folders {
            let d = check_folder(&f).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
            assert!(d.id.starts_with("reelforge.") && !d.description.is_empty() && !d.params.is_empty(), "{}", d.id);
        }
    }

    #[test]
    fn installs_from_folders_and_zips_replaces_and_removes() {
        let _turn = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tmp("install_root");
        let src = tmp("install_src");
        let mut m = manager(&root);
        m.scan();
        write_plugin(&src.join("glow-thing"), "t.inst", GOOD);
        assert_eq!(m.install_folder(&src.join("glow-thing")).unwrap(), "Installed t.inst name 1.0");
        assert!(root.join("glow-thing/effect.wgsl").is_file());
        // Installing a newer copy replaces it (same folder, no duplicate).
        std::fs::write(src.join("glow-thing/effect.wgsl"), format!("{GOOD}\n// v2")).unwrap();
        m.install_folder(&src.join("glow-thing")).unwrap();
        assert_eq!(plugin_folders(&root).len(), 1);
        assert!(std::fs::read_to_string(root.join("glow-thing/effect.wgsl")).unwrap().contains("v2"));
        // A broken plugin isn't installed.
        write_plugin(&src.join("broken"), "t.broken", "nonsense");
        assert!(m.install_folder(&src.join("broken")).is_err());
        assert!(!root.join("broken").exists());

        // A zip with the plugin inside a folder (and a sneaky path that's ignored).
        let zip_path = src.join("p.zip");
        {
            let f = std::fs::File::create(&zip_path).unwrap();
            let mut z = zip::ZipWriter::new(f);
            let o: zip::write::SimpleFileOptions = Default::default();
            z.start_file("zipped-fx/plugin.toml", o).unwrap();
            std::io::Write::write_all(&mut z, b"id = \"t.zipped\"\nname = \"Zipped\"\n").unwrap();
            z.start_file("zipped-fx/effect.wgsl", o).unwrap();
            std::io::Write::write_all(&mut z, b"fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }").unwrap();
            z.start_file("../escape.txt", o).unwrap();
            std::io::Write::write_all(&mut z, b"no").unwrap();
            z.finish().unwrap();
        }
        assert_eq!(m.install_zip(&zip_path).unwrap(), "Installed Zipped");
        assert!(root.join("zipped-fx/plugin.toml").is_file());
        assert!(!root.join("../escape.txt").exists() && !root.parent().unwrap().join("escape.txt").exists());
        assert!(plugin::find("t.zipped").is_some());
        assert!(!root.read_dir().unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with('.')), "no temporary folders left");

        // The template is a working plugin.
        let made = m.new_from_template().unwrap();
        let e = m.entries.iter().find(|e| e.folder == made).unwrap();
        assert!(e.result.is_ok(), "{:?}", e.result);
        assert_eq!(e.result.as_ref().unwrap().name, "My Effect");
        assert_eq!(e.result.as_ref().unwrap().params.len(), 4);

        // Remove.
        assert_eq!(m.remove(&root.join("zipped-fx")).unwrap(), "Removed Zipped");
        assert!(!root.join("zipped-fx").exists() && plugin::find("t.zipped").is_none());
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&src).unwrap();
    }
}
