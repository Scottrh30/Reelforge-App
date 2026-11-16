# reelforge-vp8

Pure-Rust (no `unsafe`, no dependencies) VP8 decoder, written from the VP8 specification,
[RFC 6386](https://www.rfc-editor.org/rfc/rfc6386). The standard's constant tables
(probabilities, quantiser steps) are in `src/tables.rs`, extracted from libvpx (BSD-3-Clause,
The WebM Project authors) by [`tools/tables.py`](tools/tables.py); see
`THIRD-PARTY-NOTICES.txt` at the top of the repository. ffmpeg (with libvpx) is used only as a
fixture generator and test oracle.

## Status

| Area | Status |
|---|---|
| Frame tag, key frame header, first-partition header | done |
| Boolean decoder; frame-local probability updates (refresh_entropy_probs) | done |
| Segmentation (quantiser and loop filter per segment, absolute or delta; persistent maps) | done |
| All intra modes (16x16, B_PRED subblocks with key frame contexts, chroma); 127 / 129 edges | done |
| Inter: near vector search with sign bias, NEAREST / NEAR / ZERO / NEW, SPLITMV (16x8, 8x16, 8x8, 4x4) | done |
| 6-tap (version 0) and bilinear (versions 1-3) interpolation; whole-pixel chroma (version 3) | done |
| Coefficient tokens, DCT and Walsh-Hadamard inverses, Y2 | done |
| Normal and simple loop filters, sharpness, reference / mode deltas | done |
| Golden / alternate reference refreshes, copies and sign bias; hidden frames | done |
| Multiple token partitions; odd sizes | done |

Output is 8-bit 4:2:0. Damaged data gives an error or a wrong picture, never a crash.

## Tests

`cargo test -p reelforge-vp8` encodes 24 configurations with libvpx through the ffmpeg CLI
(`target/fixtures/vp8`) and compares every decoded sample with ffmpeg's VP8 decoder (and, for
the reference-buffer tricks, libvpx's own decoder). Skipped when ffmpeg isn't installed.
`tests/robustness.rs` feeds damaged frames; `cargo test -p reelforge-vp8 --test perf -- --ignored
--nocapture` measures speed (about 110 fps for 720p on one core).
