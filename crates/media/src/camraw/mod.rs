//! Camera RAW: Blackmagic RAW, RED R3D and ARRIRAW, through the camera makers' own software.
//!
//! - **Blackmagic RAW** (`.braw`) decodes through Blackmagic's SDK, loaded at run time
//!   ([`braw`]): frames come straight from it, at the clip's own settings.
//! - **R3D** (`.r3d`) and **ARRIRAW** (`.ari`, `.arx`, ARRIRAW `.mxf`) have no SDK that can be
//!   loaded at run time (both are C++ libraries to be compiled in, and ARRI's is for partners
//!   only), so their makers' free command-line converters do it instead: REDline (part of
//!   REDCINE-X PRO) and ARRI Reference Tool CMD (`art-cmd`). Each clip is converted once, in
//!   the background, to Apple ProRes 4444 XQ in Rec.709 (Avid DNxHR 444 where REDline can't
//!   write ProRes), kept in the media cache like a proxy. Everything that reads the clip
//!   (viewer, export, thumbnails) then reads that file in its place ([`stand_in`]); the
//!   project keeps pointing at the original.
//!
//! None of this could be tried here: no camera RAW software or footage was available.

pub mod braw;

use anyhow::{anyhow, bail, Context as _, Result};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawKind {
    Braw,
    R3d,
    Arri,
}

impl RawKind {
    pub fn label(self) -> &'static str {
        match self {
            RawKind::Braw => "Blackmagic RAW",
            RawKind::R3d => "RED R3D",
            RawKind::Arri => "ARRIRAW",
        }
    }
}

fn ext(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// Is `path` camera RAW footage, and which?
pub fn kind(path: &Path) -> Option<RawKind> {
    match ext(path).as_str() {
        "braw" => Some(RawKind::Braw),
        "r3d" => Some(RawKind::R3d),
        "ari" | "arx" => Some(RawKind::Arri),
        "mxf" if is_arriraw_mxf(path) => Some(RawKind::Arri),
        _ => None,
    }
}

/// An MXF file whose picture essence is ARRIRAW (or ARRICORE), which FFmpeg can't decode.
/// Looks for SMPTE RDD 54's ARRIRAW essence label and ARRI's name in the header.
fn is_arriraw_mxf(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut head = vec![0u8; 256 * 1024];
    let n = f.read(&mut head).unwrap_or(0);
    let head = &head[..n];
    let has = |needle: &[u8]| head.windows(needle.len()).any(|w| w == needle);
    // The picture essence's coding label (06.0E.2B.34 …, ARRI's registered node 0E.1A).
    (has(b"ARRIRAW") || has(b"ARRICORE") || has(&[0x0e, 0x1a, 0x02])) && has(b"ARRI")
}

// ---- The makers' converters ----------------------------------------------------------------

/// A maker's converter found on this computer.
#[derive(Clone, Debug)]
pub struct Tool {
    pub name: &'static str,
    pub path: PathBuf,
}

fn on_path(names: &[&str]) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| p.is_file())
}

fn program_files() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = ["ProgramFiles", "ProgramW6432"].iter().filter_map(std::env::var_os).map(PathBuf::from).collect();
    v.push(PathBuf::from(r"C:\Program Files"));
    v
}

/// REDline: `REELFORGE_REDLINE`, the PATH, or where REDCINE-X PRO installs it.
pub fn redline() -> Option<Tool> {
    let mut c: Vec<PathBuf> = std::env::var_os("REELFORGE_REDLINE").map(PathBuf::from).into_iter().collect();
    c.extend(on_path(&["REDline", "REDline.exe", "redline"]));
    for pf in program_files() {
        c.push(pf.join(r"REDCINE-X PRO 64-bit\REDline.exe"));
        c.push(pf.join(r"RED Digital Cinema\REDCINE-X PRO 64-bit\REDline.exe"));
    }
    c.push("/Applications/REDCINE-X Professional/REDCINE-X PRO.app/Contents/MacOS/REDline".into());
    c.push("/usr/local/bin/REDline".into());
    c.push("/opt/REDline/REDline".into());
    c.into_iter().find(|p| p.is_file()).map(|path| Tool { name: "REDline", path })
}

/// ARRI Reference Tool CMD: `REELFORGE_ART_CMD`, the PATH, or where ARRI installs it.
pub fn art_cmd() -> Option<Tool> {
    let mut c: Vec<PathBuf> = std::env::var_os("REELFORGE_ART_CMD").map(PathBuf::from).into_iter().collect();
    c.extend(on_path(&["art-cmd", "art-cmd.exe"]));
    for pf in program_files() {
        c.push(pf.join(r"ARRI\ARRI Reference Tool CMD\art-cmd.exe"));
        c.push(pf.join(r"ARRI\ARRI Reference Tool\art-cmd.exe"));
        c.push(pf.join(r"ARRI Reference Tool CMD\art-cmd.exe"));
    }
    c.push("/Applications/ARRI Reference Tool.app/Contents/MacOS/art-cmd".into());
    c.push("/Applications/art-cmd/art-cmd".into());
    c.push("/usr/local/bin/art-cmd".into());
    c.push("/opt/art-cmd/bin/art-cmd".into());
    c.into_iter().find(|p| p.is_file()).map(|path| Tool { name: "ARRI Reference Tool", path })
}

/// The converter for footage of `kind` (None for Blackmagic RAW, which needs none).
pub fn tool_for(kind: RawKind) -> Option<Tool> {
    match kind {
        RawKind::Braw => None,
        RawKind::R3d => redline(),
        RawKind::Arri => art_cmd(),
    }
}

/// What to install to open footage of `kind`.
pub fn what_to_install(kind: RawKind) -> &'static str {
    match kind {
        RawKind::Braw => "Blackmagic RAW Player or DaVinci Resolve (free from Blackmagic Design)",
        RawKind::R3d => "REDCINE-X PRO (free from RED), which includes REDline",
        RawKind::Arri => "ARRI Reference Tool CMD (free from ARRI)",
    }
}

// ---- Converted files -----------------------------------------------------------------------

/// Where the converted file for `src` goes (whether or not it exists yet): the media cache,
/// named after the file and a hash of its path, size and date, so a changed file is converted
/// again and other projects using the same file find it.
pub fn converted_path(src: &Path, extension: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::path::absolute(src).unwrap_or_else(|_| src.to_owned()).hash(&mut h);
    if let Ok(m) = std::fs::metadata(src) {
        m.len().hash(&mut h);
        if let Ok(t) = m.modified() {
            t.hash(&mut h);
        }
    }
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let stem: String = stem.chars().filter(|c| c.is_alphanumeric() || "-_ ".contains(*c)).take(40).collect();
    crate::proxy::media_cache_dir().join("camera-raw").join(format!("{stem}-{:016x}.{extension}", h.finish()))
}

const CONVERTED_EXTS: [&str; 2] = ["mov", "mxf"];

/// The file read in place of R3D or ARRIRAW footage `src`, once it's been converted.
pub fn stand_in(src: &Path) -> Option<PathBuf> {
    match kind(src)? {
        RawKind::Braw => None,
        RawKind::R3d | RawKind::Arri => CONVERTED_EXTS.iter().map(|e| converted_path(src, e)).find(|p| p.is_file()),
    }
}

/// Does `src` need converting before it can be opened (R3D or ARRIRAW not converted yet)?
pub fn needs_conversion(src: &Path) -> Option<RawKind> {
    kind(src).filter(|k| *k != RawKind::Braw && stand_in(src).is_none())
}

/// Convert R3D or ARRIRAW footage `src` with its maker's tool; returns the converted file.
/// `progress` gets 0–1 when the tool reports it. Stops (and cleans up) when `stop` is set.
pub fn convert(src: &Path, stop: &AtomicBool, mut progress: impl FnMut(f32)) -> Result<PathBuf> {
    let kind = kind(src).ok_or_else(|| anyhow!("{} isn't R3D or ARRIRAW", src.display()))?;
    if let Some(p) = stand_in(src) {
        return Ok(p);
    }
    let tool = tool_for(kind).ok_or_else(|| anyhow!("{} footage needs {}", kind.label(), what_to_install(kind)))?;
    let work = converted_path(src, "work");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).with_context(|| format!("couldn't make {}", work.display()))?;
    let result = (|| {
        let attempts: Vec<Vec<std::ffi::OsString>> = match kind {
            RawKind::R3d => {
                let base = |v: &[&str]| -> Vec<std::ffi::OsString> {
                    let mut a: Vec<std::ffi::OsString> = vec!["--i".into(), src.into(), "--outDir".into(), work.clone().into(), "--o".into(), "converted".into()];
                    // IPP2 colour science, Rec.709 primaries, BT.1886 gamma: as RED's own
                    // monitoring shows it, with the clip's own ISO, white balance and look.
                    a.extend(["--colorSciVersion", "3", "--colorSpace", "13", "--gammaCurve", "32"].map(Into::into));
                    a.extend(v.iter().map(Into::into));
                    a
                };
                // Apple ProRes 4444 XQ; then Avid DNxHR 444 where REDline can't write ProRes.
                vec![base(&["--format", "201", "--PRcodec", "5"]), base(&["--format", "204", "--dnxCodec", "0"])]
            }
            RawKind::Arri => {
                // A clip of single-frame files is read as its folder.
                let input: PathBuf = if matches!(ext(src).as_str(), "ari" | "arx") { src.parent().map(Path::to_path_buf).unwrap_or_else(|| src.to_owned()) } else { src.to_owned() };
                let mut out = work.clone().into_os_string();
                out.push(std::path::MAIN_SEPARATOR_STR);
                vec![["process", "--input"].map(Into::into).into_iter().chain([input.into_os_string()]).chain(["--target-colorspace", "Rec.709/D65/BT.1886", "--video-codec", "prores4444xq", "--output"].map(Into::into)).chain([out]).collect()]
            }
            RawKind::Braw => bail!("Blackmagic RAW needs no conversion"),
        };
        let mut last_err = None;
        for args in attempts {
            match run_tool(&tool, &args, stop, &mut progress) {
                Ok(()) => {
                    let made = biggest_video(&work).ok_or_else(|| anyhow!("{} finished but made no file", tool.name))?;
                    let e = ext(&made);
                    let e = if CONVERTED_EXTS.contains(&e.as_str()) { e } else { "mov".to_string() };
                    let dst = converted_path(src, &e);
                    if let Some(d) = dst.parent() {
                        std::fs::create_dir_all(d)?;
                    }
                    std::fs::rename(&made, &dst).or_else(|_| std::fs::copy(&made, &dst).map(drop)).with_context(|| format!("couldn't keep {}", dst.display()))?;
                    return Ok(dst);
                }
                Err(e) if stop.load(Ordering::Relaxed) => return Err(e),
                Err(e) => {
                    log::warn!("{}: {e:#}", tool.name);
                    last_err = Some(e);
                    let _ = std::fs::remove_dir_all(&work);
                    let _ = std::fs::create_dir_all(&work);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("{} didn't run", tool.name)))
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// The largest movie file under `dir` (the tools name their output themselves).
fn biggest_video(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(u64, PathBuf)> = None;
    let mut stack = vec![dir.to_path_buf()];
    let mut seen = 0;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > 10_000 {
                break;
            }
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if ["mov", "mxf", "mp4"].contains(&ext(&p).as_str()) {
                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                if best.as_ref().is_none_or(|(l, _)| len > *l) {
                    best = Some((len, p));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// The last percentage in a line of a tool's output ("42%", "42.5 %").
fn percent(line: &str) -> Option<f32> {
    let i = line.rfind('%')?;
    let head = line[..i].trim_end();
    let start = head.rfind(|c: char| !(c.is_ascii_digit() || c == '.')).map_or(0, |k| k + 1);
    let v: f32 = head.get(start..)?.parse().ok()?;
    (0.0..=100.0).contains(&v).then_some(v / 100.0)
}

fn run_tool(tool: &Tool, args: &[std::ffi::OsString], stop: &AtomicBool, progress: &mut impl FnMut(f32)) -> Result<()> {
    let mut cmd = Command::new(&tool.path);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().with_context(|| format!("couldn't start {}", tool.path.display()))?;
    // Both streams are read on their own threads (so neither fills up and stalls the tool);
    // percentages come back here, and the last lines are kept for an error message.
    let (tx, rx) = crossbeam_channel::unbounded::<String>();
    let readers: Vec<_> = [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)]
        .into_iter()
        .flatten()
        .map(|s| {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let r = std::io::BufReader::new(s);
                for line in r.split(b'\n').map_while(|l| l.ok()) {
                    // Progress lines often end in carriage returns.
                    for part in String::from_utf8_lossy(&line).split('\r') {
                        if !part.trim().is_empty() && tx.send(part.trim().to_string()).is_err() {
                            return;
                        }
                    }
                }
            })
        })
        .collect();
    drop(tx);
    let mut tail: std::collections::VecDeque<String> = Default::default();
    let status = loop {
        while let Ok(line) = rx.try_recv() {
            if let Some(p) = percent(&line) {
                progress(p);
            }
            tail.push_back(line);
            if tail.len() > 6 {
                tail.pop_front();
            }
        }
        if stop.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("stopped");
        }
        match child.try_wait()? {
            Some(s) => break s,
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    for r in readers {
        let _ = r.join();
    }
    while let Ok(line) = rx.try_recv() {
        tail.push_back(line);
        if tail.len() > 6 {
            tail.pop_front();
        }
    }
    if !status.success() {
        let said: Vec<String> = tail.into_iter().collect();
        bail!("{} failed ({status}): {}", tool.name, said.join(" / "));
    }
    Ok(())
}

/// One line per camera RAW format on what opens it here (for Settings).
pub fn status() -> Vec<String> {
    let found = |what: &str, p: &Path| format!("{what}: {}", p.display());
    vec![
        if braw::available() { "Blackmagic RAW: through Blackmagic's SDK (installed)".to_string() } else { format!("Blackmagic RAW: install {}", what_to_install(RawKind::Braw)) },
        match redline() {
            Some(t) => found("RED R3D: converted with REDline", &t.path),
            None => format!("RED R3D: install {}", what_to_install(RawKind::R3d)),
        },
        match art_cmd() {
            Some(t) => found("ARRIRAW: converted with ARRI Reference Tool", &t.path),
            None => format!("ARRIRAW: install {}", what_to_install(RawKind::Arri)),
        },
    ]
}

// ---- Blackmagic RAW as media ----------------------------------------------------------------

/// What a Blackmagic RAW clip is (for the project's media list).
pub fn braw_info(path: &Path) -> Result<reelforge_core::MediaInfo> {
    let clip = braw::Clip::open(path)?;
    let rate = reelforge_core::adobe::rate_of(f64::from(clip.fps));
    let duration = reelforge_core::Time::from_frame(clip.frames as i64, rate);
    Ok(reelforge_core::MediaInfo {
        duration,
        video: Some(reelforge_core::VideoStreamInfo {
            width: clip.width,
            height: clip.height,
            frame_rate: rate,
            codec: "braw".into(),
            time_base: reelforge_core::Rational::new(rate.0.den, rate.0.num),
            hdr: None,
            pixel_aspect: (1, 1),
        }),
        audio: None,
        container: "braw".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_name_and_content() {
        assert_eq!(kind(Path::new("A001_C002.braw")), Some(RawKind::Braw));
        assert_eq!(kind(Path::new("A001_C002_0101AB_001.R3D")), Some(RawKind::R3d));
        assert_eq!(kind(Path::new("A001C001_150101_R1ZA.0000001.ari")), Some(RawKind::Arri));
        assert_eq!(kind(Path::new("clip.mov")), None);
        // An MXF that isn't ARRIRAW (or doesn't exist) is left to FFmpeg.
        assert_eq!(kind(Path::new("/nonexistent/clip.mxf")), None);
        let dir = std::env::temp_dir().join(format!("rf-camraw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let m = dir.join("C_0001C001.mxf");
        let mut head = vec![0x06, 0x0e, 0x2b, 0x34];
        head.extend(b"....ARRI ALEXA 35....ARRIRAW....");
        std::fs::write(&m, &head).unwrap();
        assert_eq!(kind(&m), Some(RawKind::Arri));
        std::fs::write(&m, b"\x06\x0e\x2b\x34 plain mxf").unwrap();
        assert_eq!(kind(&m), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn percentages_from_tool_output() {
        assert_eq!(percent("Processing frame 12/48 (25%)"), Some(0.25));
        assert_eq!(percent("progress: 42.5 %"), Some(0.425));
        assert_eq!(percent("no numbers"), None);
        assert_eq!(percent("150%"), None);
    }

    #[test]
    fn converted_files_are_named_per_source() {
        let a = converted_path(Path::new("/footage/A001.R3D"), "mov");
        let b = converted_path(Path::new("/footage/B001.R3D"), "mov");
        assert_ne!(a, b);
        assert!(a.to_string_lossy().contains("camera-raw") && a.extension().is_some_and(|e| e == "mov"));
        assert!(stand_in(Path::new("/footage/never-converted.R3D")).is_none());
        assert_eq!(needs_conversion(Path::new("/footage/never-converted.R3D")), Some(RawKind::R3d));
        assert_eq!(needs_conversion(Path::new("/footage/x.braw")), None);
    }

    /// A stand-in converter (a script playing REDline) shows the whole round trip: its
    /// progress is read, its output kept as the converted file, and that file is then what
    /// opens in the original's place.
    #[cfg(unix)]
    #[test]
    fn a_converter_runs_and_its_output_stands_in() {
        use std::os::unix::fs::PermissionsExt;
        let Some(ff) = reelforge_testkit_ffmpeg() else { return };
        let dir = std::env::temp_dir().join(format!("rf-camraw-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("A001_C001.R3D");
        std::fs::write(&src, b"not really R3D").unwrap();
        let fake = dir.join("REDline");
        // Writes a ProRes 4444 XQ file into --outDir as REDline would, reporting progress.
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in --outDir) out=\"$2\"; shift;; --format) fmt=\"$2\"; shift;; esac; shift; done\n[ \"$fmt\" = 201 ] || exit 3\necho 'Processing 50%'\n{} -loglevel error -f lavfi -i testsrc2=size=320x180:rate=24:duration=1 -c:v prores_ks -profile:v 5 \"$out/converted.mov\" || exit 1\necho 'Done 100%'\n",
                ff.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: tests that change this variable don't run concurrently with others reading it.
        unsafe { std::env::set_var("REELFORGE_REDLINE", &fake) };
        let mut seen = Vec::new();
        let out = convert(&src, &AtomicBool::new(false), |p| seen.push(p)).unwrap();
        assert!(seen.contains(&0.5) && seen.contains(&1.0), "{seen:?}");
        assert_eq!(stand_in(&src).as_deref(), Some(out.as_path()));
        assert!(needs_conversion(&src).is_none());
        // Now it opens like any other footage, under the original's name.
        let info = crate::probe(&src).unwrap();
        let v = info.video.unwrap();
        assert_eq!((v.width, v.height, v.codec.as_str()), (320, 180, "prores"));
        let mut s = crate::FrameSource::open(&src, None).unwrap();
        let f = s.frame_at_blocking(reelforge_core::Time::ZERO, Duration::from_secs(5)).unwrap();
        assert_eq!((f.width, f.height), (320, 180));
        // A tool that fails gives its last words as the error.
        std::fs::write(&fake, "#!/bin/sh\necho 'License not accepted' >&2\nexit 2\n").unwrap();
        let src2 = dir.join("B001_C001.R3D");
        std::fs::write(&src2, b"x").unwrap();
        let e = convert(&src2, &AtomicBool::new(false), |_| {}).unwrap_err().to_string();
        assert!(e.contains("License not accepted"), "{e}");
        unsafe { std::env::remove_var("REELFORGE_REDLINE") };
        let _ = std::fs::remove_file(&out);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    fn reelforge_testkit_ffmpeg() -> Option<PathBuf> {
        ["/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg", "/opt/homebrew/bin/ffmpeg"].iter().map(PathBuf::from).find(|p| p.exists())
    }
}
