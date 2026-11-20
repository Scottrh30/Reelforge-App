#!/bin/sh
# Real DNx files FFmpeg can't make (12-bit DNxHR HQX; DNxHD CID 1260,
# adaptive interlace): FFmpeg's own test samples (fate-suite), checked by
# SHA-256, into target/fixtures/dnx for crates/media's dnx_samples test.
# Not committed: they're FFmpeg's test material, fetched when wanted.
set -e
cd "$(dirname "$0")/../.."
d=target/fixtures/dnx
mkdir -p $d
fetch() {
  [ -f "$d/$1" ] && echo "$2  $d/$1" | sha256sum -c --quiet && return
  curl -sfL -o "$d/$1" "https://fate-suite.ffmpeg.org/dnxhd/$1" ||
    curl -sfL -o "$d/$1" "https://raw.githubusercontent.com/luke-cha/fate-suite/gh-pages/video/dnxhd/${1%.mov}.mp4"
  echo "$2  $d/$1" | sha256sum -c --quiet
}
fetch dnxhr_cid1271_12bit.mov 8126766c535b24b49c0502757f9f19c862a19534217889be1feba3c65317d84f
fetch dnxhd100_cid1260.mov e0f38194b223b85264c2bfb95ab72cb839229803bce11a59f37166e63b5c3a9a
echo ok
