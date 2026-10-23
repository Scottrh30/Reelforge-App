# CineForm test clips

`crates/media/tests/data/cineform/` holds small clips made with GoPro's own
CineForm encoder (the open-source CineForm SDK, github.com/gopro/cineform-sdk),
because FFmpeg's encoder only makes intra frames and FFmpeg's decoder can't
read the SDK's two-frame groups. The decoder tests check that each group clip
decodes to the same pictures as its intra twin (same source frames).

To remake them, build the SDK (`cmake` then `make`, giving `libCFHDCodec.a`), then:

```
g++ -O1 -o cfenc cfenc.cpp -I$SDK/Common -I$SDK/EncoderSDK -I$SDK/Codec $BUILD/libCFHDCodec.a -lpthread -luuid
# cfenc W H FRAMES yuv|rgb|rgba GOP QUALITY INTERLACED out.cfs
./cfenc 256 144 3 yuv 2 2 0 cf-group.cfs && python3 mkavi.py cf-group.cfs 256 144 cf-group.avi
./cfenc 256 144 3 yuv 2 2 1 cf-group-interlaced.cfs && python3 mkavi.py cf-group-interlaced.cfs 256 144 cf-group-interlaced.avi
./cfenc 256 144 2 yuv 1 2 0 cf-intra.cfs && python3 mkavi.py cf-intra.cfs 256 144 cf-intra.avi
./cfenc 256 144 2 yuv 1 2 1 cf-intra-interlaced.cfs && python3 mkavi.py cf-intra-interlaced.cfs 256 144 cf-intra-interlaced.avi
```

The SDK's encoder writes a sequence header as its first sample in group mode
(no picture), then each group, then the group's second frame.
