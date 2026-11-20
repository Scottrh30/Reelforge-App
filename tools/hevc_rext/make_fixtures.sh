#!/bin/bash
# HEVC range-extension test streams for crates/hevc's rext test, made with the
# HEVC reference software (HM 16.x, BSD licence; FFmpeg can't use or make all
# of these tools) from FFmpeg test patterns, and HM's own decoding of each as
# the expected result (a 64-bit FNV-1a hash per frame of its Y, U, V planes,
# 16-bit little-endian samples above 8 bits).
#
#   HM=/path/to/HM/bin tools/hevc_rext/make_fixtures.sh crates/hevc/tests/data/rext
set -e
out=$1; mkdir -p "$out"
enc=$(ls $HM/TAppEncoder*Static | head -1); dec=$(ls $HM/TAppDecoder*Static | head -1)
cfg=$(dirname $HM)/cfg
tmp=$(mktemp -d)
W=96; H=64; N=4
make() { # name format bitdepth cfg options...
  local name=$1 fmt=$2 bd=$3 c=$4; shift 4
  local pf=yuv${fmt}p; [ $bd -gt 8 ] && pf=${pf}${bd}le
  ffmpeg -v error -y -f lavfi -i "testsrc2=size=${W}x${H}:rate=25,noise=alls=12:allf=t" -frames:v $N -pix_fmt $pf -f rawvideo $tmp/in.yuv
  $enc -c $cfg/$c -i $tmp/in.yuv -wdt $W -hgt $H -fr 25 -f $N --InputBitDepth=$bd --InternalBitDepth=$bd --InputChromaFormat=$fmt --ChromaFormatIDC=$fmt \
    --ConformanceWindowMode=1 -b $out/$name.bit -o /dev/null "$@" > $tmp/enc.log || { cat $tmp/enc.log; exit 1; }
  $dec -b $out/$name.bit -o $tmp/rec.yuv -d $bd > /dev/null
  python3 - "$tmp/rec.yuv" $W $H $fmt $bd $N > $out/$name.hash <<'PY'
import sys
p, w, h, fmt, bd, n = sys.argv[1], *map(int, sys.argv[2:])
b = 2 if bd > 8 else 1
cw, ch = {444: (w, h), 422: (w // 2, h), 420: (w // 2, h // 2)}[fmt]
size = (w * h + 2 * cw * ch) * b
d = open(p, 'rb').read()
assert len(d) == size * n, (len(d), size * n)
for i in range(n):
    h = 0xcbf29ce484222325
    for b in d[i * size:(i + 1) * size]:
        h = ((h ^ b) * 0x100000001b3) & 0xffffffffffffffff
    print('%016x' % h)
PY
  echo "$name ok"
}
# Intra: implicit RDPCM, rotation, single significance context, Rice adaptation,
# transform skip up to 32x32, cross-component prediction.
make intra_444_8 444 8 encoder_intra_main_rext.cfg -q 22 --TransformSkipLog2MaxSize=5 --CrossComponentPrediction=1
make intra_444_10_nosmooth 444 10 encoder_intra_main_rext.cfg -q 27 --IntraReferenceSmoothing=0 --TransformSkipLog2MaxSize=3
# Lossless: transquant bypass with implicit RDPCM (no intra boundary filters).
make lossless_444_8 444 8 encoder_intra_main_rext.cfg -q 22 --TransquantBypassEnableFlag=1 --CUTransquantBypassFlagForce=1
# Inter: explicit RDPCM, cross-component prediction, chroma QP offset lists,
# high-precision weighted prediction.
make ld_444_10 444 10 encoder_lowdelay_main_rext.cfg -q 30 --MaxCUChromaQpAdjustmentDepth=1 --WeightedPredP=1 --WeightedPredB=1
make ld_lossless_444_8 444 8 encoder_lowdelay_main_rext.cfg -q 30 --TransquantBypassEnableFlag=1 --CUTransquantBypassFlagForce=1
make ld_422_10 422 10 encoder_lowdelay_main_rext.cfg -q 28 --TransformSkipLog2MaxSize=4
# Extended precision and CABAC bypass alignment (the high-throughput tools), and
# SAO offset scaling, 12-bit.
make extprec_444_12 444 12 encoder_intra_main_rext.cfg -q 12 --ExtendedPrecision=1 --TransformSkipLog2MaxSize=4
make align_444_12 444 12 encoder_intra_high_throughput_rext.cfg -q 15 --ExtendedPrecision=1
make ld_extprec_444_12 444 12 encoder_lowdelay_main_rext.cfg -q 20 --ExtendedPrecision=1 --SaoLumaOffsetBitShift=2 --SaoChromaOffsetBitShift=1
rm -rf $tmp
