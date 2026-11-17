#!/bin/sh
# Fuzz every target (or the ones named) for SECONDS each (default 600), two at a time.
# Crashes, hangs and memory blow-ups land in artifacts/<target>/.
# Build first: RUSTC_BOOTSTRAP=1 cargo fuzz build -s none -O
cd "$(dirname "$0")"
SECONDS_EACH=${SECONDS_EACH:-600}
bin=target/x86_64-unknown-linux-gnu/release
targets=${*:-"h264 hevc vp9 av1 vp8 mpeg2 prores dnxhd cineform exr mjpeg prproj aep project media"}
run() {
    mkdir -p corpus/$1 artifacts/$1
    # (seeds/<target>: extra starting inputs kept in the repository, e.g. field-coded H.264 from
    # the JM reference encoder, which FFmpeg can't make)
    $bin/$1 corpus/$1 $( [ -d seeds/$1 ] && echo seeds/$1 ) -artifact_prefix=artifacts/$1/ -max_total_time=$SECONDS_EACH -timeout=10 \
        -rss_limit_mb=2048 -malloc_limit_mb=1536 -max_len=262144 -print_final_stats=1 > logs/$1.log 2>&1
    echo "$1: $(grep -c 'artifact_prefix\|Test unit written' logs/$1.log) findings, $(grep -E '^#[0-9]+.*DONE' logs/$1.log | tail -1)"
}
mkdir -p logs
set -- $targets
while [ $# -gt 0 ]; do
    run $1 &
    if [ $# -gt 1 ]; then run $2 & fi
    wait
    shift; [ $# -gt 0 ] && shift
done
