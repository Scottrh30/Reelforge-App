#!/usr/bin/env python3
"""Write THIRD-PARTY-NOTICES.txt: every library built into the Windows app (and
the interface's bundled packages and fonts), with its licence and the licence
texts it ships, copyright lines included.

Run from the repository root (needs cargo, and `npm install` done in the UI):
    python3 tools/notices.py [--target x86_64-pc-windows-gnu] [--out THIRD-PARTY-NOTICES.txt]

What counts: the normal (run-time) dependencies of reelforge-tauri for the
target, without compile-time-only crates (proc macros, build scripts' helpers),
which don't end up in the program. Reelforge's own crates are left out.
"""
import argparse, json, os, re, subprocess, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LICENSE_FILE = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE|COPYRIGHT|UNLICENSE|AUTHORS)([-_.].*)?$", re.I)


def run(*cmd, cwd=ROOT):
    return subprocess.run(cmd, cwd=cwd, check=True, capture_output=True, text=True).stdout


def licence_texts(folder: Path):
    """The licence files at the top of a package folder (and one level down in LICENSES/)."""
    found = []
    if not folder.is_dir():
        return found
    candidates = [p for p in folder.iterdir() if p.is_file() and LICENSE_FILE.match(p.name)]
    for sub in ("LICENSES", "licenses", "license"):
        d = folder / sub
        if d.is_dir():
            candidates += [p for p in d.iterdir() if p.is_file()]
    # Licences of bundled assets (fonts…) one folder down.
    for d in folder.iterdir():
        if d.is_dir() and d.name not in ("LICENSES", "licenses", "license", "src", "tests", "benches", "examples", "target"):
            candidates += [p for p in d.iterdir() if p.is_file() and (LICENSE_FILE.match(p.name) or "licen" in p.name.lower() or p.name.upper().startswith("OFL"))]
    for p in sorted(candidates):
        try:
            text = p.read_text(encoding="utf-8", errors="replace").strip()
        except OSError:
            continue
        if text:
            found.append((p.name, text))
    return found


def rust_packages(target):
    meta = json.loads(run("cargo", "metadata", "--format-version", "1", "--filter-platform", target))
    pkgs = {p["id"]: p for p in meta["packages"]}
    workspace = set(meta["workspace_members"])
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = next(i for i in workspace if pkgs[i]["name"] == "reelforge-tauri")
    is_macro = lambda p: any("proc-macro" in t["kind"] for t in p["targets"])
    # Walk normal dependencies only (not build or dev).
    seen, todo = set(), [root]
    while todo:
        i = todo.pop()
        if i in seen:
            continue
        seen.add(i)
        for d in nodes[i]["deps"]:
            if any(k["kind"] is None for k in d["dep_kinds"]) and not is_macro(pkgs[d["pkg"]]):
                todo.append(d["pkg"])
    out = []
    for i in seen:
        if i in workspace:
            continue
        p = pkgs[i]
        folder = Path(p["manifest_path"]).parent
        lic = p.get("license") or ""
        if not lic and p.get("license_file"):
            lic = "see licence file"
        out.append({"name": p["name"], "version": p["version"], "license": lic, "repo": p.get("repository") or "", "authors": p.get("authors") or [], "texts": licence_texts(folder)})
    return sorted(out, key=lambda x: (x["name"].lower(), x["version"]))


# Standard texts (licenses/standard/<SPDX id>.txt, from the SPDX licence list) and
# words that show a library's own files already include that licence.
STANDARD = {
    "MIT": "Permission is hereby granted, free of charge",
    "Apache-2.0": "Apache License",
    "BSD-3-Clause": "Redistribution and use in source and binary forms",
    "MPL-2.0": "Mozilla Public License",
    "WTFPL": "DO WHAT THE FUCK YOU WANT",
    "OFL-1.1": "SIL OPEN FONT LICENSE",
    "Ubuntu-font-1.0": "UBUNTU FONT LICENCE",
}


def needed_standard(p):
    """Licences a package must carry but ships no text for: for an OR choice,
    the first one we have; every part of an AND."""
    expr = p["license"].replace("/", " OR ")
    have = " ".join(t for _, t in p["texts"]).upper()
    need = []
    for part in re.split(r"\bAND\b", expr):
        options = [o.strip(" ()") for o in re.split(r"\bOR\b", part) if o.strip(" ()")]
        if any(STANDARD.get(o, "\0").upper() in have for o in options):
            continue
        pick = next((o for o in options if o in STANDARD), None)
        if pick:
            need.append(pick)
    return need


def ui_packages():
    """Packages whose code or files end up in the built interface."""
    ui = ROOT / "crates" / "tauri-app" / "ui"
    out = []
    for name in ("svelte", "@tauri-apps/api", "@fontsource/ibm-plex-sans", "esm-env", "clsx"):
        d = ui / "node_modules" / name
        pj = d / "package.json"
        if not pj.is_file():
            continue
        info = json.loads(pj.read_text())
        repo = info.get("repository")
        if isinstance(repo, dict):
            repo = repo.get("url", "")
        author = info.get("author")
        if isinstance(author, dict):
            author = author.get("name")
        out.append({"name": name, "version": info.get("version", ""), "license": info.get("license", ""), "repo": repo or "", "authors": [author] if author else [], "texts": licence_texts(d)})
    return out


def write(path, rust, ui):
    lines = []
    w = lines.append
    w("Third-party notices for Reelforge")
    w("=================================")
    w("")
    w("Reelforge is built from the open-source software listed here. Each part keeps")
    w("its own licence; the licence texts and copyright notices they ship are below.")
    w("")
    head = ROOT / "THIRD-PARTY-NOTICES.head.txt"
    if head.is_file():
        w(head.read_text().strip())
        w("")
    w("FFmpeg")
    w("------")
    w("")
    w("The FFmpeg libraries (av*.dll, sw*.dll) are downloaded separately by")
    w("\"Get FFmpeg.bat\" and are licensed under the GNU GPL version 3 (see")
    w("licenses/FFmpeg-LICENSE.txt). Source: https://ffmpeg.org.")
    w("")
    w("The Rust standard library and runtime")
    w("-------------------------------------")
    w("")
    w("Parts of the Rust standard library are built into the program: MIT or")
    w("Apache-2.0, Copyright (c) The Rust Project Developers. The Windows build links")
    w("the MinGW-w64 runtime (winpthreads: MIT/BSD-style; libgcc under the GCC Runtime")
    w("Library Exception), whose notices allow this without further requirements.")
    w("")
    w("OpenFX")
    w("------")
    w("")
    w("Reelforge's OpenFX host follows the OpenFX standard's API headers (BSD-3-Clause,")
    w("Copyright OpenFX and contributors to the OpenFX project, now the Academy Software")
    w("Foundation; https://github.com/AcademySoftwareFoundation/openfx). Its data")
    w("layouts and names come from them; no OpenFX code is built in.")
    w("")
    w("Source code: every library below can be had from its repository (listed")
    w("beside it) or from crates.io / npmjs.com at the version given. That includes")
    w("the MPL-2.0 ones (cssparser, selectors, dtoa-short, option-ext), used unchanged.")
    w("")
    w(f"Rust libraries ({len(rust)})")
    w("-" * len(f"Rust libraries ({len(rust)})"))
    w("")
    for p in rust:
        w(f"  {p['name']} {p['version']}  —  {p['license'] or 'licence not stated'}" + (f"  ({p['repo']})" if p["repo"] else ""))
    w("")
    w(f"Interface packages and fonts ({len(ui)})")
    w("-" * len(f"Interface packages and fonts ({len(ui)})"))
    w("")
    for p in ui:
        w(f"  {p['name']} {p['version']}  —  {p['license']}" + (f"  ({p['repo']})" if p["repo"] else ""))
    w("")
    w("")
    w("Licence texts")
    w("=============")
    w("")
    w("Each library's own licence files, as it ships them. Identical texts are given")
    w("once, with every library that uses them listed above the text.")
    w("")
    for f in sorted((ROOT / "licenses").glob("FilmCraft-*.txt")):
        w("-" * 78)
        w(f"FilmCraft ({f.name}): the H.264, HEVC, VP9 and AV1 decoders")
        w("-" * 78)
        w(f.read_text(encoding="utf-8").strip())
        w("")
    for f in sorted((ROOT / "licenses").glob("CineForm-SDK-*.txt")):
        w("-" * 78)
        w(f"GoPro CineForm SDK ({f.name}): the CineForm decoder's code tables")
        w("-" * 78)
        w(f.read_text(encoding="utf-8").strip())
        w("")
    # Group identical texts.
    groups = {}
    order = []
    missing = []
    for p in rust + ui:
        if not p["texts"]:
            missing.append(p)
        for fname, text in p["texts"]:
            key = text
            if key not in groups:
                groups[key] = []
                order.append(key)
            groups[key].append(f"{p['name']} {p['version']} ({fname})")
    for key in order:
        users = groups[key]
        w("-" * 78)
        w("Used by: " + ", ".join(users))
        w("-" * 78)
        w(key)
        w("")
    # Standard texts for the licences some libraries name but ship no text for.
    std_users = {}
    for p in rust + ui:
        for lid in needed_standard(p):
            who = ", ".join(a.split("<")[0].strip() for a in p["authors"]) or "its authors"
            std_users.setdefault(lid, []).append(f"{p['name']} {p['version']} (Copyright (c) {who})")
    for lid in sorted(std_users):
        text = (ROOT / "licenses" / "standard" / f"{lid}.txt").read_text(encoding="utf-8").strip()
        w("-" * 78)
        w(f"{lid}, the standard text, for libraries that name it but ship no copy:")
        for u in std_users[lid]:
            w(f"  {u}")
        w("-" * 78)
        w(text)
        w("")
    missing = [p for p in missing if not needed_standard(p) and not p["texts"]]
    Path(path).write_text("\n".join(lines) + "\n", encoding="utf-8")
    return len(order), missing


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--target", default="x86_64-pc-windows-gnu")
    ap.add_argument("--out", default=str(ROOT / "THIRD-PARTY-NOTICES.txt"))
    a = ap.parse_args()
    rust = rust_packages(a.target)
    ui = ui_packages()
    n, missing = write(a.out, rust, ui)
    print(f"{len(rust)} Rust libraries, {len(ui)} interface packages, {n} licence texts; no licence file: {len(missing)}", file=sys.stderr)
    for p in missing:
        print(f"  no licence file: {p['name']} {p['version']} ({p['license']})", file=sys.stderr)


if __name__ == "__main__":
    main()
