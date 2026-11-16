#!/usr/bin/env python3
"""Make seed inputs for the fuzz targets: tiny clips of each codec, in many variants, made
with ffmpeg and cut into packets (and setup data) with ffprobe, in the input format of
src/lib.rs. Usage: python3 seeds.py [out dir, default corpus]"""
import os, re, struct, subprocess, sys, tempfile

OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "corpus")
SRC = "testsrc2=size={size}:rate=25:duration={dur},format={pf}"

def x264(*a): return ["-c:v", "libx264", "-preset", "veryfast", *a]
def x265(*a): return ["-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "log-level=error", *a]

# (target, name, size, frames, pixel format, ffmpeg args, extension)
RECIPES = [
    ("mjpeg", "basic", "96x64", 3, "yuvj420p", ["-c:v", "mjpeg"], "avi"),
    ("mjpeg", "422", "96x64", 2, "yuvj422p", ["-c:v", "mjpeg"], "mov"),
    ("prores", "proxy", "96x64", 2, "yuv422p10le", ["-c:v", "prores_ks", "-profile:v", "0"], "mov"),
    ("prores", "hq", "96x64", 2, "yuv422p10le", ["-c:v", "prores_ks", "-profile:v", "3"], "mov"),
    ("prores", "4444a", "96x64", 2, "yuva444p10le", ["-c:v", "prores_ks", "-profile:v", "4"], "mov"),
    ("prores", "interlaced", "96x64", 2, "yuv422p10le", ["-c:v", "prores_ks", "-flags", "+ildct"], "mov"),
    ("dnxhd", "lb", "256x128", 2, "yuv422p", ["-c:v", "dnxhd", "-profile:v", "dnxhr_lb"], "mov"),
    ("dnxhd", "hqx", "256x128", 2, "yuv422p10le", ["-c:v", "dnxhd", "-profile:v", "dnxhr_hqx"], "mov"),
    ("dnxhd", "444", "256x128", 2, "yuv444p10le", ["-c:v", "dnxhd", "-profile:v", "dnxhr_444"], "mov"),
    ("dnxhd", "dnxhd", "1920x1080", 1, "yuv422p", ["-c:v", "dnxhd", "-b:v", "36M"], "mov"),
    ("mpeg2", "ipb", "96x64", 8, "yuv420p", ["-c:v", "mpeg2video", "-bf", "2", "-g", "6"], "m2v"),
    ("mpeg2", "interlaced", "96x64", 6, "yuv420p", ["-c:v", "mpeg2video", "-flags", "+ildct+ilme", "-bf", "2"], "m2v"),
    ("mpeg2", "422", "96x64", 4, "yuv422p", ["-c:v", "mpeg2video", "-alternate_scan", "1", "-intra_vlc", "1"], "m2v"),
    ("h264", "cabac_mp4", "96x64", 8, "yuv420p", x264("-bf", "2"), "mp4"),
    ("h264", "cavlc_ts", "96x64", 8, "yuv420p", x264("-x264-params", "cabac=0:slices=2"), "ts"),
    ("h264", "high_8x8", "96x64", 8, "yuv420p", x264("-profile:v", "high", "-x264-params", "weightp=2:b-pyramid=normal"), "mp4"),
    ("h264", "mbaff", "96x64", 6, "yuv420p", x264("-flags", "+ildct+ilme", "-x264-params", "interlaced=1:bframes=2"), "mp4"),
    ("h264", "hi10", "96x64", 6, "yuv420p10le", x264("-profile:v", "high10"), "mp4"),
    ("h264", "hi422", "96x64", 6, "yuv422p", x264("-profile:v", "high422"), "mp4"),
    ("hevc", "main_mp4", "96x64", 8, "yuv420p", x265("-bf", "2"), "mp4"),
    ("hevc", "ts", "96x64", 6, "yuv420p", x265(), "ts"),
    ("hevc", "main10", "96x64", 6, "yuv420p10le", x265(), "mp4"),
    ("hevc", "422", "96x64", 4, "yuv422p10le", x265(), "mp4"),
    ("hevc", "444", "96x64", 4, "yuv444p", x265(), "mp4"),
    ("vp8", "good", "96x64", 8, "yuv420p", ["-c:v", "libvpx", "-b:v", "200k"], "ivf"),
    ("vp8", "altref_parts", "96x64", 10, "yuv420p", ["-c:v", "libvpx", "-b:v", "200k", "-auto-alt-ref", "1", "-lag-in-frames", "6", "-slices", "4"], "ivf"),
    ("vp8", "rt_profile3", "96x64", 8, "yuv420p", ["-c:v", "libvpx", "-deadline", "realtime", "-cpu-used", "8", "-profile:v", "3"], "ivf"),
    ("vp9", "p0", "96x64", 8, "yuv420p", ["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-b:v", "200k"], "ivf"),
    ("vp9", "p2_10bit", "96x64", 6, "yuv420p10le", ["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-profile:v", "2"], "ivf"),
    ("vp9", "p1_444", "96x64", 6, "yuv444p", ["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-profile:v", "1"], "ivf"),
    ("av1", "aom", "96x64", 6, "yuv420p", ["-c:v", "libaom-av1", "-cpu-used", "8", "-crf", "40"], "ivf"),
    ("av1", "aom_444", "96x64", 4, "yuv444p10le", ["-c:v", "libaom-av1", "-cpu-used", "8", "-crf", "40"], "ivf"),
    ("cineform", "422", "96x64", 2, "yuv422p10le", ["-c:v", "cfhd"], "mov"),
    ("cineform", "rgba", "96x64", 2, "gbrap12le", ["-c:v", "cfhd"], "mov"),
    ("exr", "zip", "48x32", 1, "gbrpf32le", ["-c:v", "exr", "-compression", "zip16"], "exr"),
    ("exr", "zip1_half", "48x32", 1, "gbrapf32le", ["-c:v", "exr", "-compression", "zip1", "-format", "half"], "exr"),
    ("exr", "rle", "48x32", 1, "gbrpf32le", ["-c:v", "exr", "-compression", "rle"], "exr"),
]

def hexdump(block):
    """Bytes from ffprobe's hex dump lines ("00000000: 0000 0001 ...  ascii")."""
    out = bytearray()
    for line in block:
        m = re.match(r"^[0-9a-f]{8}: ((?:[0-9a-f]{2,4} ?)+)", line)
        if m:
            out += bytes.fromhex(m.group(1).replace(" ", ""))
    return bytes(out)

def dump(path, what):
    """[extradata] or [packet data...] from ffprobe."""
    args = ["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_data"]
    args += ["-show_streams"] if what == "stream" else ["-show_packets"]
    text = subprocess.run(args + [path], capture_output=True, text=True, check=True).stdout.splitlines()
    key = "extradata=" if what == "stream" else "data="
    blocks, cur = [], None
    for line in text:
        if line.startswith(key):
            cur = []
            blocks.append(cur)
        elif cur is not None and re.match(r"^[0-9a-f]{8}: ", line):
            cur.append(line)
        else:
            cur = None
    return [hexdump(b) for b in blocks]

def record_file(w, h, extradata, packets):
    v = struct.pack("<HH", w, h)
    for r in [extradata] + packets:
        v += struct.pack("<I", len(r)) + r
    return v

def main():
    made = 0
    with tempfile.TemporaryDirectory() as tmp:
        for target, name, size, frames, pf, args, ext in RECIPES:
            src = os.path.join(tmp, f"{target}-{name}.{ext}")
            vf = SRC.format(size=size, dur=frames / 25, pf=pf)
            cmd = ["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", vf, "-frames:v", str(frames), *args, "-pix_fmt", pf, src]
            if subprocess.run(cmd).returncode != 0:
                print("skipped", target, name)
                continue
            ex = dump(src, "stream")
            packets = [p for p in dump(src, "packet") if p]
            if ext == "exr":
                packets = [open(src, "rb").read()]
            w, h = (int(x) for x in size.split("x"))
            os.makedirs(os.path.join(OUT, target), exist_ok=True)
            with open(os.path.join(OUT, target, f"seed-{name}"), "wb") as f:
                f.write(record_file(w, h, ex[0] if ex else b"", packets))
            made += 1
    print(f"{made} seeds in {OUT}")

main()
