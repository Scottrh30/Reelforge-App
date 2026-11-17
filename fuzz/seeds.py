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
    media_seeds()
    project_seeds()


# Whole media files for the media target: its first byte picks the extension
# (mp4, mov, mxf, webm, mkv, r3d, braw, ari), the rest is the file.
MEDIA = [
    (0, "h264_aac", ["-c:v", "libx264", "-preset", "veryfast", "-c:a", "aac", "-b:a", "32k"], "mp4"),
    (1, "prores_pcm", ["-c:v", "prores_ks", "-profile:v", "0", "-c:a", "pcm_s16le"], "mov"),
    (1, "h264_rotated", ["-c:v", "libx264", "-preset", "veryfast", "-metadata:s:v", "rotate=90", "-an"], "mov"),
    (2, "dnxhd_pcm", ["-c:v", "dnxhd", "-profile:v", "dnxhr_lb", "-c:a", "pcm_s24le"], "mxf"),
    (2, "mpeg2_pcm", ["-c:v", "mpeg2video", "-c:a", "pcm_s16le"], "mxf"),
    (3, "vp9_opus", ["-c:v", "libvpx-vp9", "-b:v", "100k", "-c:a", "libopus", "-b:a", "24k"], "webm"),
    (3, "vp8_alpha", ["-c:v", "libvpx", "-pix_fmt", "yuva420p", "-auto-alt-ref", "0", "-an"], "webm"),
    (4, "hevc_flac", ["-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "log-level=error", "-c:a", "flac"], "mkv"),
]


def media_seeds():
    out = os.path.join(OUT, "media")
    os.makedirs(out, exist_ok=True)
    n = 0
    with tempfile.TemporaryDirectory() as tmp:
        for pick, name, args, ext in MEDIA:
            path = os.path.join(tmp, f"{name}.{ext}")
            pf = ["-pix_fmt", "yuv420p"] if "-pix_fmt" not in args and "prores_ks" not in args and "dnxhd" not in args else []
            if "dnxhd" in args:
                pf = ["-pix_fmt", "yuv422p"]
            if "prores_ks" in args:
                pf = ["-pix_fmt", "yuv422p10le"]
            size = "256x128" if "dnxhd" in args else "64x48"
            cmd = ["ffmpeg", "-loglevel", "error", "-y", "-f", "lavfi", "-i", f"testsrc2=size={size}:rate=25:duration=0.2",
                   "-f", "lavfi", "-i", "sine=frequency=440:duration=0.2:sample_rate=48000", *pf, *args, "-shortest", path]
            if subprocess.run(cmd).returncode != 0 or not os.path.exists(path):
                print(f"media {name}: ffmpeg couldn't make it")
                continue
            data = open(path, "rb").read()
            open(os.path.join(out, f"seed-{name}"), "wb").write(bytes([pick]) + data)
            n += 1
    # Camera RAW names: what's checked before anything is decoded.
    for pick, name, head in [(5, "r3d", b"\x00\x00\x00\x1cRED2" + bytes(64)), (6, "braw", b"\x00\x00\x00\x08ftypbraw" + bytes(64)), (7, "ari", b"ARRI\x12\x34\x56\x78" + bytes(4088))]:
        open(os.path.join(out, f"seed-{name}"), "wb").write(bytes([pick]) + head)
        n += 1
    print(f"{n} media seeds")


def project_seeds():
    """Premiere, After Effects and Reelforge projects: the samples the importers' tests use."""
    root = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
    with tempfile.TemporaryDirectory() as tmp:
        env = dict(os.environ, REELFORGE_FUZZ_SEEDS=tmp)
        subprocess.run(["cargo", "test", "-q", "-p", "reelforge-core", "--lib", "adobe"], cwd=root, env=env, stdout=subprocess.DEVNULL)
        for target, files in [("prproj", ["sample.prproj", "sample.prproj.xml"]), ("aep", ["sample.aep"]), ("project", ["from-premiere.rfproj", "from-after-effects.rfproj"])]:
            out = os.path.join(OUT, target)
            os.makedirs(out, exist_ok=True)
            for f in files:
                src = os.path.join(tmp, f)
                if os.path.exists(src):
                    open(os.path.join(out, "seed-" + f), "wb").write(open(src, "rb").read())
    print("project seeds made")

if __name__ == "__main__":
    main()
