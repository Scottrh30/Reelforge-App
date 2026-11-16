#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| reelforge_fuzz::run(reelforge_codecs::Codec::Vp8, data));
