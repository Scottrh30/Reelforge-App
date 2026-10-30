# OpenFX SDK examples (test fixtures)

The OpenFX standard's headers and four of its example plugins (Invert,
Basic "Gain", Rectangle and ChoiceParams), unchanged, from
https://github.com/AcademySoftwareFoundation/openfx (commit in `VERSION`),
under the BSD 3-Clause licence in `LICENSE.md`.

`crates/ofx/tests/sdk_plugins.rs` builds them with the system C++ compiler
and checks Reelforge hosts them correctly. They aren't part of the program.
