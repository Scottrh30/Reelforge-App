# OpenFX SDK examples (test fixtures)

The OpenFX standard's headers and five of its example plugins (Invert,
Basic "Gain", Rectangle, ChoiceParams and OpenGL), unchanged, from
https://github.com/AcademySoftwareFoundation/openfx (commit in `VERSION`),
under the BSD 3-Clause licence in `LICENSE.md`.

`crates/ofx/tests/sdk_plugins.rs` builds them with the system C++ compiler
and checks Reelforge hosts them correctly (the OpenGL one on whatever OpenGL
the machine has; Mesa's software renderer will do). They aren't part of the
program. `../plugins/` holds Reelforge's own test plugins: `frameavg.cpp`
(reads other frames), `clgain.cpp` (OpenCL kernels, buffers or images),
`cudagain.cpp` with `fakecuda.c` (a stand-in for NVIDIA's CUDA driver, so the
host's CUDA path can be tested without an NVIDIA card).
