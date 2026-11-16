Interlaced H.264 test streams made with the JM reference encoder (12.1, field pictures,
picture-adaptive frame/field, MBAFF, temporal and spatial direct, CAVLC and CABAC; FFmpeg's
encoders make none of these), 176x144 from FFmpeg's `testsrc2` (alternating moving
interlaced and still stretches so JM picks both frame and field pictures).
`expected.txt` holds a hash of FFmpeg's decode of each; `tests/jm_interlaced.rs` checks
Reelforge decodes them the same.
