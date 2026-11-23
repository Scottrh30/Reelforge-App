#!/usr/bin/env bash
# Builds the Windows download: Reelforge-Windows-x64.zip (reelforge.exe, the
# example plugins, PLUGINS.md, the README, licences and Get FFmpeg.bat).
#
#   tools/package-windows.sh OUT.zip
#
# Needs a release build at target/x86_64-pc-windows-gnu/release/reelforge.exe:
#   python3 tools/notices.py --target x86_64-pc-windows-gnu   (built into the program)
#   cargo build --release -p reelforge-tauri --target x86_64-pc-windows-gnu
# Used by the
# GitHub workflow (.github/workflows/build.yml) and for local builds alike.
set -euo pipefail
out="$(realpath -m "${1:?usage: tools/package-windows.sh OUT.zip}")"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
exe=target/x86_64-pc-windows-gnu/release/reelforge.exe
[ -f "$exe" ] || { echo "no $exe: build it first" >&2; exit 1; }
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
p="$stage/Reelforge"
mkdir -p "$p"
cp "$exe" "$p/"
cp -r plugins licenses "$p/"
cp PLUGINS.md README.md THIRD-PARTY-NOTICES.txt LICENSE-MIT LICENSE-APACHE NOTICE "$p/"
cp "tools/windows/Get FFmpeg.bat" "$p/"
# NVIDIA RTX Video (optional, never in the repo: NVIDIA's SDK isn't open source):
# REELFORGE_RTX_VIDEO names a folder holding reelforge_rtxvideo.dll (built against the
# RTX Video SDK), NVIDIA's nvngx_vsr.dll and nvngx_truehdr.dll, and the SDK's licence.
if [ -n "${REELFORGE_RTX_VIDEO:-}" ]; then
  for f in reelforge_rtxvideo.dll nvngx_vsr.dll nvngx_truehdr.dll; do cp "$REELFORGE_RTX_VIDEO/$f" "$p/"; done
  cp "$REELFORGE_RTX_VIDEO/NVIDIA RTX SDKs License.pdf" "$p/licenses/" 2>/dev/null || true
fi
rm -f "$out"
(cd "$stage" && zip -qr "$out" Reelforge)
echo "$out"
