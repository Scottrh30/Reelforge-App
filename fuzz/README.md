# Fuzzing the decoders and file readers

Every native decoder (MJPEG, ProRes, DNxHD, MPEG-2, H.264, HEVC, VP8, VP9, AV1, CineForm,
OpenEXR) has a [libFuzzer](https://llvm.org/docs/LibFuzzer.html) target here. A target feeds
mutated streams through `reelforge_codecs::open` exactly as the app does (setup data, then
packets, a flush now and then, then finish) and reports any panic, hang (10 s) or allocation
over 1.5 GB.

The file readers have targets too, since a damaged file can take the app down just as easily
as a damaged stream:

| Target | Input | What it goes through |
|---|---|---|
| `media` | first byte picks the extension (MP4, MOV, MXF, WebM, MKV, R3D, BRAW, ARI); the rest is the file | camera RAW detection, probing, the first and a middle picture (Reelforge's own decoders where they apply, else FFmpeg's), the sound at two places, the waveform |
| `prproj` | a Premiere Pro project (gzipped or plain XML) | `adobe::prproj::read`, then the project walked |
| `aep` | an After Effects project (RIFX) | `adobe::aep::read`, then the project walked |
| `project` | a Reelforge project (JSON) | `serde_json` into `Project`, path resolution, then the project walked |

"The project walked" (`src/files.rs`, `exercise`) is what the app does once a project is open:
every sequence's pictures worked out at four moments with the export's scene walk (nested
sequences included), each clip's timing, speed, transform, effects, title and mesh at those
moments, the edit points, and saving it again.

Unpacking MP4, MOV, MXF and WebM is FFmpeg's C code, which these targets reach but which isn't
Reelforge's to fix; what they test is everything Reelforge does with what FFmpeg hands it. The
`media` target links FFmpeg, so its libraries must be on `LD_LIBRARY_PATH` when it runs.

This is its own Cargo workspace, so the app's build never sees it.

## Running

```sh
cargo install cargo-fuzz
cd fuzz
python3 seeds.py                          # starting streams, media files and projects → corpus/
                                          # (needs ffmpeg + ffprobe, and cargo for the project samples)
RUSTC_BOOTSTRAP=1 cargo fuzz build -s none -O    # or use a nightly toolchain
SECONDS_EACH=600 ./run.sh                 # every target, two at a time; or ./run.sh h264 vp9
```

Add `-a` to the build for debug assertions (integer overflow checks). Findings land in
`artifacts/<target>/`; reproduce one with
`target/x86_64-unknown-linux-gnu/release/<target> artifacts/<target>/crash-…`.

## Input format (decoders)

Two little-endian `u16`s (width, height), then records of a little-endian `u32` length and
that many bytes. The first record is the container's setup data (avcC, hvcC, sequence
headers…), the rest are packets (at most 48).

## Regressions

When a finding is fixed, copy it to `regressions/<target>/` (short, descriptive name).
`cargo test -p reelforge-codecs --test fuzz_regressions` runs every decoder file there through the
same harness, so it stays fixed; `cargo test -p reelforge-export --test fuzz_regressions` does
the same for `media`, `prproj`, `aep` and `project` (it includes `src/files.rs`, so it runs the
very same steps), and reads each media file ten more times to catch memory kept per read.
