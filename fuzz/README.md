# Fuzzing the decoders

Every native decoder (MJPEG, ProRes, DNxHD, MPEG-2, H.264, HEVC, VP8, VP9, AV1, CineForm,
OpenEXR) has a [libFuzzer](https://llvm.org/docs/LibFuzzer.html) target here. A target feeds
mutated streams through `reelforge_codecs::open` exactly as the app does (setup data, then
packets, a flush now and then, then finish) and reports any panic, hang (10 s) or allocation
over 1.5 GB.

This is its own Cargo workspace, so the app's build never sees it.

## Running

```sh
cargo install cargo-fuzz
cd fuzz
python3 seeds.py                          # starting streams (needs ffmpeg + ffprobe) → corpus/
RUSTC_BOOTSTRAP=1 cargo fuzz build -s none -O    # or use a nightly toolchain
SECONDS_EACH=600 ./run.sh                 # every target, two at a time; or ./run.sh h264 vp9
```

Add `-a` to the build for debug assertions (integer overflow checks). Findings land in
`artifacts/<target>/`; reproduce one with
`target/x86_64-unknown-linux-gnu/release/<target> artifacts/<target>/crash-…`.

## Input format

Two little-endian `u16`s (width, height), then records of a little-endian `u32` length and
that many bytes. The first record is the container's setup data (avcC, hvcC, sequence
headers…), the rest are packets (at most 48).

## Regressions

When a finding is fixed, copy it to `regressions/<target>/` (short, descriptive name).
`cargo test -p reelforge-codecs --test fuzz_regressions` runs every file there through the
same harness, so it stays fixed.
