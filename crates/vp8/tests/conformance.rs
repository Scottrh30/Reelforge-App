//! Bit-exactness against ffmpeg's VP8 decoder on libvpx fixtures (skipped without ffmpeg and
//! libvpx).

mod common;

macro_rules! fixture_tests {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                match common::check_fixture(stringify!($name)) {
                    Ok(true) => {}
                    Ok(false) => eprintln!("skipped {}", stringify!($name)),
                    Err(e) => panic!("{e}"),
                }
            }
        )*
    };
}

fixture_tests!(
    intra_only,
    default_good,
    motion_best,
    motion_good,
    motion_realtime,
    altref_hidden,
    altref_motion,
    partitions_2,
    partitions_8,
    profile1,
    profile2,
    profile3,
    odd_size,
    tiny_odd,
    one_mb,
    q_low,
    q_high,
    sharpness,
    error_resilient,
    screen_segments,
    noise_sensitive,
    temporal_layers,
    fade_intra,
    hd_720p,
);

/// libvpx's own decoder agrees with ffmpeg's on every fixture that could tell them apart
/// (reference buffer updates, temporal layers): ours matches both.
#[test]
fn matches_libvpx_too() {
    for name in ["temporal_layers", "altref_hidden", "profile3", "screen_segments"] {
        let f = common::fixture(name);
        let Some((ivf, _)) = common::ensure(f) else { return };
        let yuv = common::fixtures_dir().join(format!("{name}.libvpx.yuv"));
        if !yuv.exists() && !common::reference_decode(&ivf, &yuv, "libvpx") {
            eprintln!("skipped: no libvpx decoder");
            return;
        }
        let reference = std::fs::read(&yuv).unwrap();
        let pics = common::decode_file(&ivf).unwrap();
        common::compare(&pics, &reference).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}
