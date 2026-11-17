#![no_main]
//! Media files (MP4, MOV, MXF, WebM, MKV and camera RAW names) through probing,
//! decoding, sound and waveforms.
libfuzzer_sys::fuzz_target!(|data: &[u8]| reelforge_fuzz::run_media(data));
