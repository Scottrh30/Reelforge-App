//! "Finish install.exe": completes a Reelforge download that comes in parts.
//!
//! The Windows build is shipped as several zips (files sent through chat are
//! limited in size, and FFmpeg's codec library alone is bigger than that).
//! Part 1 holds Reelforge itself and this program; the other parts hold the
//! rest of FFmpeg, with the biggest DLL cut into pieces. This program:
//!
//! 1. finds the other parts (next to it, in the folder above, or in the
//!    Downloads folder; a browser's "(1)" copies are fine, the newest wins),
//! 2. takes the FFmpeg files out of them (or uses ones already unzipped here),
//! 3. joins the pieces back together,
//! 4. checks every file's size and checksum against `ffmpeg-files.txt`,
//! 5. tidies up and offers to start Reelforge.
//!
//! It touches nothing outside its own folder, and never deletes the zips.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Lists the files that must end up here: `name  size  crc32` per line.
const MANIFEST: &str = "ffmpeg-files.txt";
const ZIP_PREFIX: &str = "Reelforge-Windows-x64-part";

#[derive(Debug, Clone, PartialEq)]
struct Expect {
    name: String,
    size: u64,
    crc: u32,
}

fn read_manifest(text: &str) -> Result<(usize, Vec<Expect>), String> {
    let mut parts = 0;
    let mut v = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f.as_slice() {
            ["parts", n] => parts = n.parse().map_err(|_| format!("bad line in {MANIFEST}: {line}"))?,
            [name, size, crc] => v.push(Expect {
                name: name.to_string(),
                size: size.parse().map_err(|_| format!("bad size in {MANIFEST}: {line}"))?,
                crc: u32::from_str_radix(crc, 16).map_err(|_| format!("bad checksum in {MANIFEST}: {line}"))?,
            }),
            _ => return Err(format!("bad line in {MANIFEST}: {line}")),
        }
    }
    if parts < 2 || v.is_empty() {
        return Err(format!("{MANIFEST} is incomplete"));
    }
    Ok((parts, v))
}

/// Size and CRC-32 of a file.
fn checksum(path: &Path) -> std::io::Result<(u64, u32)> {
    let mut f = std::fs::File::open(path)?;
    let mut h = crc32fast::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((n, h.finalize()))
}

fn good(dir: &Path, e: &Expect) -> bool {
    checksum(&dir.join(&e.name)).is_ok_and(|(n, c)| n == e.size && c == e.crc)
}

/// Which part a zip's file name is ("Reelforge-Windows-x64-part3-of-4 (1).zip" -> 3).
fn part_number(file_name: &str) -> Option<usize> {
    let lower = file_name.to_lowercase();
    if !lower.ends_with(".zip") || !lower.starts_with(&ZIP_PREFIX.to_lowercase()) {
        return None;
    }
    let rest = &file_name[ZIP_PREFIX.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Folders to look in for the part zips.
fn search_dirs(here: &Path) -> Vec<PathBuf> {
    let mut v = vec![here.to_path_buf()];
    if let Some(p) = here.parent() {
        v.push(p.to_path_buf());
        if let Some(g) = p.parent() {
            v.push(g.to_path_buf());
        }
    }
    for var in ["USERPROFILE", "HOME"] {
        if let Some(h) = std::env::var_os(var) {
            let d = PathBuf::from(h).join("Downloads");
            if !v.contains(&d) {
                v.push(d);
            }
        }
    }
    v
}

/// The newest zip found for each part number.
fn find_parts(dirs: &[PathBuf]) -> BTreeMap<usize, PathBuf> {
    let mut found: BTreeMap<usize, (std::time::SystemTime, PathBuf)> = BTreeMap::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(n) = part_number(&name) else { continue };
            let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            if found.get(&n).is_none_or(|(old, _)| t > *old) {
                found.insert(n, (t, e.path()));
            }
        }
    }
    found.into_iter().map(|(n, (_, p))| (n, p)).collect()
}

/// Take the files out of a part zip into `dir` (flat: only file names, so a
/// zip can't write anywhere else). Returns how many it took.
fn extract(zip_path: &Path, dir: &Path, wanted: &dyn Fn(&str) -> bool, say: &mut dyn FnMut(&str)) -> Result<usize, String> {
    let f = std::fs::File::open(zip_path).map_err(|e| format!("can't open {}: {e}", zip_path.display()))?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| format!("{} isn't a complete zip ({e}): download it again", file_name(zip_path)))?;
    let mut n = 0;
    for i in 0..z.len() {
        let mut entry = z.by_index(i).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            continue;
        }
        let Some(name) = entry.enclosed_name().and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned())) else { continue };
        if !wanted(&name) {
            continue;
        }
        say(&format!("  unpacking {name}"));
        let tmp = dir.join(format!("{name}.unpacking"));
        let mut out = std::fs::File::create(&tmp).map_err(|e| format!("can't write {name}: {e}"))?;
        // The zip checks each file's own checksum as it's read.
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("{} is damaged ({e}): download it again", file_name(zip_path)))?;
        drop(out);
        let dest = dir.join(&name);
        let _ = std::fs::remove_file(&dest);
        std::fs::rename(&tmp, &dest).map_err(|e| format!("can't write {name}: {e}"))?;
        n += 1;
    }
    Ok(n)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The pieces of `name` here: name.part1, name.part2, … in order.
fn pieces(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for k in 1.. {
        let p = dir.join(format!("{name}.part{k}"));
        if !p.is_file() {
            break;
        }
        v.push(p);
    }
    v
}

/// Join a file's pieces into it.
fn join(dir: &Path, name: &str, pieces: &[PathBuf]) -> std::io::Result<()> {
    let tmp = dir.join(format!("{name}.joining"));
    {
        let mut out = std::fs::File::create(&tmp)?;
        for p in pieces {
            let mut f = std::fs::File::open(p)?;
            std::io::copy(&mut f, &mut out)?;
        }
        out.flush()?;
    }
    let dest = dir.join(name);
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&tmp, dest)
}

/// Do the whole job in folder `dir`, looking for zips in `dirs`.
/// `say` prints progress. Ok(true) if something was installed, Ok(false) if
/// everything was already there.
fn finish(dir: &Path, dirs: &[PathBuf], say: &mut dyn FnMut(&str)) -> Result<bool, String> {
    let text = std::fs::read_to_string(dir.join(MANIFEST)).map_err(|_| format!("{MANIFEST} is missing: run this from the Reelforge folder that came out of part 1"))?;
    let (parts, files) = read_manifest(&text)?;
    let missing = |files: &[Expect]| files.iter().filter(|e| !good(dir, e)).cloned().collect::<Vec<_>>();
    let mut todo = missing(&files);
    if todo.is_empty() {
        return Ok(false);
    }
    // Pieces already here (unzipped by hand, or left from an earlier run) are tried first.
    let join_here = |todo: &[Expect], say: &mut dyn FnMut(&str)| -> Result<(), String> {
        for e in todo {
            let p = pieces(dir, &e.name);
            if !p.is_empty() && !good(dir, e) {
                say(&format!("Joining {} pieces into {}", p.len(), e.name));
                join(dir, &e.name, &p).map_err(|err| format!("couldn't join {}: {err}", e.name))?;
            }
        }
        Ok(())
    };
    join_here(&todo, say)?;
    todo = missing(&files);
    if !todo.is_empty() {
        // Unpack what's still needed from the zips.
        let found = find_parts(dirs);
        let need: Vec<usize> = (2..=parts).filter(|n| !found.contains_key(n)).collect();
        if !need.is_empty() {
            let list = need.iter().map(|n| format!("{ZIP_PREFIX}{n}-of-{parts}.zip")).collect::<Vec<_>>().join(", ");
            let where_ = dirs.iter().map(|d| format!("    {}", d.display())).collect::<Vec<_>>().join("\n");
            let it = if need.len() == 1 { "it" } else { "them" };
            return Err(format!("Can't find {list}.\nDownload {it} and put {it} in your Downloads folder (or next to the Reelforge folder), then run this again.\nLooked in:\n{where_}"));
        }
        let names: Vec<String> = todo.iter().map(|e| e.name.clone()).collect();
        let wanted = |n: &str| names.iter().any(|w| n == w || n.starts_with(&format!("{w}.part")));
        for n in 2..=parts {
            let z = &found[&n];
            say(&format!("Part {n} of {parts}: {}", z.display()));
            extract(z, dir, &wanted, say)?;
        }
        join_here(&todo, say)?;
    }
    todo = missing(&files);
    if !todo.is_empty() {
        let names = todo.iter().map(|e| e.name.as_str()).collect::<Vec<_>>().join(", ");
        return Err(format!("These files didn't come out right: {names}.\nOne of the download parts is probably incomplete: download the parts again and run this again."));
    }
    // Tidy up the pieces now that the whole files check out.
    for e in &files {
        for p in pieces(dir, &e.name) {
            let _ = std::fs::remove_file(p);
        }
    }
    say(&format!("All {} FFmpeg files are in place and check out.", files.len()));
    Ok(true)
}

fn main() {
    let here = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."));
    println!("Reelforge: finishing the install");
    println!("Folder: {}\n", here.display());
    let mut say = |s: &str| println!("{s}");
    let ok = match finish(&here, &search_dirs(&here), &mut say) {
        Ok(true) => {
            println!("\nDone. Reelforge is ready.");
            true
        }
        Ok(false) => {
            println!("Everything is already in place.");
            true
        }
        Err(e) => {
            println!("\nCouldn't finish:\n{e}");
            false
        }
    };
    let exe = here.join("reelforge.exe");
    if ok && exe.is_file() && std::env::args().all(|a| a != "--no-start") {
        print!("\nStart Reelforge now? [Y/n] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        if !answer.trim().eq_ignore_ascii_case("n") {
            let _ = std::process::Command::new(&exe).current_dir(&here).spawn();
        }
    } else if std::env::args().all(|a| a != "--no-start") {
        print!("\nPress Enter to close.");
        let _ = std::io::stdout().flush();
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    if !ok {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rf_setup_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_zip(path: &Path, files: &[(&str, &[u8])]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (n, b) in files {
            z.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(b).unwrap();
        }
        z.finish().unwrap();
    }

    /// A Reelforge folder from part 1, and parts 2–3 in a "Downloads" folder:
    /// a.dll whole in part 2, big.dll cut in two across parts 2 and 3.
    fn setup(name: &str) -> (PathBuf, PathBuf, Vec<u8>, Vec<u8>) {
        let root = tmp(name);
        let app = root.join("Reelforge");
        let dl = root.join("Downloads");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(&dl).unwrap();
        let a: Vec<u8> = (0..5000u32).map(|i| (i * 7) as u8).collect();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let crc = |b: &[u8]| crc32fast::hash(b);
        std::fs::write(app.join(MANIFEST), format!("parts 3\na.dll {} {:08x}\nbig.dll {} {:08x}\n", a.len(), crc(&a), big.len(), crc(&big))).unwrap();
        make_zip(&dl.join("Reelforge-Windows-x64-part2-of-3.zip"), &[("a.dll", &a), ("big.dll.part1", &big[..150_000])]);
        make_zip(&dl.join("Reelforge-Windows-x64-part3-of-3 (1).zip"), &[("big.dll.part2", &big[150_000..]), ("../evil.dll", b"x")]);
        (app, dl, a, big)
    }

    #[test]
    fn finds_unpacks_joins_and_checks() {
        let (app, dl, a, big) = setup("ok");
        let mut log = Vec::new();
        let r = finish(&app, &[app.clone(), dl.clone()], &mut |s: &str| log.push(s.to_string()));
        assert_eq!(r, Ok(true), "{log:?}");
        assert_eq!(std::fs::read(app.join("a.dll")).unwrap(), a);
        assert_eq!(std::fs::read(app.join("big.dll")).unwrap(), big);
        assert!(!app.join("big.dll.part1").exists() && !app.join("big.dll.part2").exists(), "pieces tidied away");
        assert!(!app.join("evil.dll").exists() && !app.parent().unwrap().join("evil.dll").exists());
        assert!(dl.join("Reelforge-Windows-x64-part2-of-3.zip").exists(), "zips kept");
        // Running again: nothing to do.
        assert_eq!(finish(&app, &[dl.clone()], &mut |_| {}), Ok(false));
        // A damaged DLL is put right from the zips.
        std::fs::write(app.join("big.dll"), b"broken").unwrap();
        assert_eq!(finish(&app, &[dl.clone()], &mut |_| {}), Ok(true));
        assert_eq!(std::fs::read(app.join("big.dll")).unwrap(), big);
    }

    #[test]
    fn says_what_is_missing_and_catches_bad_downloads() {
        let (app, dl, _, big) = setup("missing");
        std::fs::remove_file(dl.join("Reelforge-Windows-x64-part3-of-3 (1).zip")).unwrap();
        let e = finish(&app, &[dl.clone()], &mut |_| {}).unwrap_err();
        assert!(e.contains("Can't find Reelforge-Windows-x64-part3-of-3.zip"), "{e}");
        // A part with the wrong contents: caught by the checksums.
        make_zip(&dl.join("Reelforge-Windows-x64-part3-of-3.zip"), &[("big.dll.part2", &big[150_000..299_000])]);
        let e = finish(&app, &[dl.clone()], &mut |_| {}).unwrap_err();
        assert!(e.contains("didn't come out right: big.dll"), "{e}");
        // A truncated zip.
        let bytes = std::fs::read(dl.join("Reelforge-Windows-x64-part2-of-3.zip")).unwrap();
        std::fs::write(dl.join("Reelforge-Windows-x64-part2-of-3.zip"), &bytes[..bytes.len() / 2]).unwrap();
        let e = finish(&app, &[dl.clone()], &mut |_| {}).unwrap_err();
        assert!(e.contains("download it again"), "{e}");
    }

    #[test]
    fn uses_files_unzipped_by_hand() {
        let (app, dl, a, big) = setup("by_hand");
        // Someone unzipped everything into the folder themselves, and deleted the zips.
        std::fs::write(app.join("a.dll"), &a).unwrap();
        std::fs::write(app.join("big.dll.part1"), &big[..150_000]).unwrap();
        std::fs::write(app.join("big.dll.part2"), &big[150_000..]).unwrap();
        std::fs::remove_dir_all(&dl).unwrap();
        assert_eq!(finish(&app, &[], &mut |_| {}), Ok(true));
        assert_eq!(std::fs::read(app.join("big.dll")).unwrap(), big);
    }

    #[test]
    fn part_names() {
        assert_eq!(part_number("Reelforge-Windows-x64-part2-of-4.zip"), Some(2));
        assert_eq!(part_number("reelforge-windows-x64-part4-of-4 (3).zip"), Some(4));
        assert_eq!(part_number("Reelforge-Windows-x64-update.zip"), None);
        assert_eq!(part_number("Reelforge-Windows-x64-part2-of-4.zip.crdownload"), None);
    }
}
