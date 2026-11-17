#![no_main]
//! Premiere Pro projects through the importer, then the whole project walked as the app would.
libfuzzer_sys::fuzz_target!(|data: &[u8]| reelforge_fuzz::run_prproj(data));
