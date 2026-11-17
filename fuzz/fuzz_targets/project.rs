#![no_main]
//! Reelforge projects (JSON) through the importer, then the whole project walked as the app would.
libfuzzer_sys::fuzz_target!(|data: &[u8]| reelforge_fuzz::run_project(data));
