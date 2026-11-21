"""Small OpenEXR test files for Reelforge's EXR decoder, written with the
OpenEXR reference library (pip install OpenEXR numpy), and that library's own
reading of each one as the expected result.

    python3 tools/exr/make_fixtures.py crates/codecs/tests/data/exr

For each NAME.exr there's NAME.ref: the picture in its display window as
little-endian float32 R, G, B, A per pixel (A = 1 without an alpha channel,
everything 0 outside the data window), as OpenEXR decodes it.
"""
import sys, os
import numpy as np
import OpenEXR

out = sys.argv[1]
os.makedirs(out, exist_ok=True)
rng = np.random.default_rng(7)


def picture(h, w, alpha=True, dtype=np.float16, hdr=40.0, noise_level=0.05):
    y, x = np.mgrid[0:h, 0:w].astype(np.float32)
    r = x / w * hdr                       # up to well above 1 (scene-linear highlights)
    g = (y / h) ** 2.2
    b = np.sin(x * 0.3) * np.cos(y * 0.2) # negative values too
    noise = rng.normal(0, noise_level, (h, w)).astype(np.float32)
    r = r + noise
    g[: h // 3, : w // 3] = 0.5           # a flat patch (runs, B44A flat blocks)
    a = np.clip(x / w + 0.2 * noise, 0, 1)
    chans = {"R": r * a, "G": g * a, "B": b * a} if alpha else {"R": r, "G": g, "B": b}
    if alpha:
        chans["A"] = a
    if dtype == np.float32:
        # Smooth floats with short mantissas, as renders often are, so they compress.
        chans = {k: np.round(v * 1024) / 1024 for k, v in chans.items()}
    return {k: v.astype(dtype) for k, v in chans.items()}


# ONLY=prefix,prefix: write just the files whose names start so (the others stay as made).
ONLY = [o for o in os.environ.get("ONLY", "").split(",") if o]


def write(name, header, channels):
    if ONLY and not any(name.startswith(o) for o in ONLY):
        return
    path = os.path.join(out, name + ".exr")
    OpenEXR.File(header, channels).write(path)
    reference(path)


def reference(path, part_names=None):
    """What the OpenEXR library reads back, as display-window RGBA float32."""
    f = OpenEXR.File(path, separate_channels=True)
    for p in f.parts:
        hdr = p.header
        ch = p.channels
        names = list(ch.keys())
        def find(base):
            for pre in [""] + sorted({n[: n.rfind(".") + 1] for n in names if "." in n}):
                for n in (pre + base, pre + base.lower()):
                    if n in ch:
                        return pre
            return None
        pre = None
        for cand in [""] + sorted({n[: n.rfind(".") + 1] for n in names if "." in n}):
            if all(cand + c in ch for c in "RGB") or cand + "Y" in ch:
                pre = cand
                break
        if pre is None:
            continue
        (dx0, dy0), (dx1, dy1) = hdr["dataWindow"]
        (wx0, wy0), (wx1, wy1) = hdr["displayWindow"]
        W, H = wx1 - wx0 + 1, wy1 - wy0 + 1
        img = np.zeros((H, W, 4), np.float32)
        if pre + "R" in ch:
            rgb = [ch[pre + c].pixels.astype(np.float32) for c in "RGB"]
        else:
            yv = ch[pre + "Y"].pixels.astype(np.float32)
            rgb = [yv, yv, yv]
        a = ch[pre + "A"].pixels.astype(np.float32) if pre + "A" in ch else None
        full = np.zeros((dy1 - dy0 + 1, dx1 - dx0 + 1, 4), np.float32)
        for i in range(3):
            full[..., i] = rgb[i]
        full[..., 3] = 1.0 if a is None else a
        # Place the data window within the display window.
        for yy in range(dy0, dy1 + 1):
            oy = yy - wy0
            if not 0 <= oy < H:
                continue
            for xx in range(dx0, dx1 + 1):
                ox = xx - wx0
                if 0 <= ox < W:
                    img[oy, ox] = full[yy - dy0, xx - dx0]
        if a is None:
            # No alpha channel: opaque everywhere the picture is.
            pass
        img.astype("<f4").tofile(path[:-4] + ".ref")
        return
    raise SystemExit(f"{path}: no colour channels")


C = OpenEXR
H, W = 37, 61
for comp, name in [(C.NO_COMPRESSION, "none"), (C.RLE_COMPRESSION, "rle"), (C.ZIPS_COMPRESSION, "zips"), (C.ZIP_COMPRESSION, "zip"),
                   (C.PIZ_COMPRESSION, "piz"), (C.PXR24_COMPRESSION, "pxr24"), (C.B44_COMPRESSION, "b44"), (C.B44A_COMPRESSION, "b44a")]:
    # Gentle noise, so every compression really compresses (noisy data gets stored as it is).
    # (PIZ's value bitmap needs a bigger picture to pay for itself.)
    size = (80, 120) if comp == C.PIZ_COMPRESSION else (H, W)
    write(f"half-rgba-{name}", {"compression": comp, "type": C.scanlineimage}, picture(*size, noise_level=0.002))
write("half-rgba-piz-noisy", {"compression": C.PIZ_COMPRESSION, "type": C.scanlineimage}, picture(H, W))

for comp, name in [(C.ZIP_COMPRESSION, "zip"), (C.PIZ_COMPRESSION, "piz"), (C.PXR24_COMPRESSION, "pxr24"), (C.B44_COMPRESSION, "b44")]:
    write(f"float-rgb-{name}", {"compression": comp, "type": C.scanlineimage}, picture(H, W, alpha=False, dtype=np.float32, noise_level=0.002))

# Bigger and noisier: long Huffman codes in PIZ.
write("float-rgba-piz-big", {"compression": C.PIZ_COMPRESSION, "type": C.scanlineimage}, picture(96, 160, dtype=np.float32, hdr=1000.0, noise_level=0.01))

# Tiled.
tile = C.TileDescription()
tile.xSize, tile.ySize = 64, 48
write("tiled-half-rgba-piz", {"compression": C.PIZ_COMPRESSION, "type": C.tiledimage, "tiles": tile}, picture(70, 150, noise_level=0.002))
tile32 = C.TileDescription()
tile32.xSize, tile32.ySize = 32, 32
write("tiled-float-rgb-zip", {"compression": C.ZIP_COMPRESSION, "type": C.tiledimage, "tiles": tile32}, picture(H, W, alpha=False, dtype=np.float32, noise_level=0.002))

# The data window inside, and partly outside, the display window.
write("datawindow-zip", {"compression": C.ZIP_COMPRESSION, "type": C.scanlineimage,
                          "dataWindow": (np.array([-5, 10], np.int32), np.array([-5 + W - 1, 10 + H - 1], np.int32)),
                          "displayWindow": (np.array([0, 0], np.int32), np.array([79, 49], np.int32))}, picture(H, W))

write("decreasing-y-zip", {"compression": C.ZIP_COMPRESSION, "type": C.scanlineimage, "lineOrder": C.DECREASING_Y}, picture(H, W))

# Luminance only.
write("luma-piz", {"compression": C.PIZ_COMPRESSION, "type": C.scanlineimage}, {"Y": picture(H, W, alpha=False)["G"]})

# Layers: beauty.R/G/B/A next to other passes.
p = picture(H, W)
layered = {"beauty." + k: v for k, v in p.items()}
layered["depth.Z"] = (np.ones((H, W)) * 12.5).astype(np.float32)
layered["N.X"] = p["R"]
write("layers-zip", {"compression": C.ZIP_COMPRESSION, "type": C.scanlineimage}, layered)

# Multi-part: depth in the first part, the picture in the second.
path = os.path.join(out, "multipart-piz.exr")
parts = [] if ONLY else [C.Part({"compression": C.ZIP_COMPRESSION, "type": C.scanlineimage}, {"Z": np.ones((H, W), np.float32)}, name="depth"),
         C.Part({"compression": C.PIZ_COMPRESSION, "type": C.scanlineimage}, picture(H, W, noise_level=0.002), name="rgba")]
if parts:
    C.File(parts).write(path)
    reference(path)

# DWAA and DWAB: lossy, so the test allows a little difference from the
# library (its float maths may round a value the other way).
write("dwaa-half-rgba", {"compression": C.DWAA_COMPRESSION, "type": C.scanlineimage}, picture(H, W))
write("dwab-half-rgba", {"compression": C.DWAB_COMPRESSION, "type": C.scanlineimage}, picture(300, 90, noise_level=0.01))
write("dwaa-float-rgb", {"compression": C.DWAA_COMPRESSION, "type": C.scanlineimage}, picture(H, W, alpha=False, dtype=np.float32))
write("dwaa-luma", {"compression": C.DWAA_COMPRESSION, "type": C.scanlineimage}, {"Y": picture(H, W, alpha=False)["G"]})
p = picture(H, W)
layered = {"beauty." + k: v for k, v in p.items()}
layered["depth.Z"] = (np.ones((H, W)) * 12.5 + rng.normal(0, 1, (H, W))).astype(np.float32)
layered["id"] = rng.integers(0, 9, (H, W)).astype(np.uint32)
write("dwaa-layers", {"compression": C.DWAA_COMPRESSION, "type": C.scanlineimage}, layered)
write("dwaa-tiled", {"compression": C.DWAA_COMPRESSION, "type": C.tiledimage, "tiles": tile32}, picture(70, 90))
write("dwab-flat", {"compression": C.DWAB_COMPRESSION, "type": C.scanlineimage, "dwaCompressionLevel": 200.0}, picture(H, W, noise_level=0.0))

# HTJ2K (OpenEXR 3.4) and LJ2K and ZSTD (3.5), written by OpenEXR 3.5.
for comp, name in [(C.HTJ2K256_COMPRESSION, "htj2k256"), (C.HTJ2K32_COMPRESSION, "htj2k32"), (C.ZSTD_COMPRESSION, "zstd")]:
    write(f"{name}-half-rgba", {"compression": comp, "type": C.scanlineimage}, picture(300 if comp == C.HTJ2K256_COMPRESSION else H, W, noise_level=0.01))
    write(f"{name}-float-rgb", {"compression": comp, "type": C.scanlineimage}, picture(H, W, alpha=False, dtype=np.float32, hdr=1000.0))
    write(f"{name}-luma", {"compression": comp, "type": C.scanlineimage}, {"Y": picture(H, W, alpha=False)["G"]})
    p = picture(H, W)
    layered = {"beauty." + k: v for k, v in p.items()}
    layered["depth.Z"] = (np.ones((H, W)) * 12.5 + rng.normal(0, 1, (H, W))).astype(np.float32)
    layered["id"] = rng.integers(0, 9, (H, W)).astype(np.uint32)
    write(f"{name}-layers", {"compression": comp, "type": C.scanlineimage}, layered)
    write(f"{name}-tiled", {"compression": comp, "type": C.tiledimage, "tiles": tile}, picture(70, 150, noise_level=0.01))
    write(f"{name}-datawindow", {"compression": comp, "type": C.scanlineimage,
                                 "dataWindow": (np.array([-5, 3], np.int32), np.array([-5 + W - 1, 3 + H - 1], np.int32)),
                                 "displayWindow": (np.array([0, 0], np.int32), np.array([79, 49], np.int32))}, picture(H, W))
# LJ2K as OpenEXR 3.5.0/3.5.1 wrote it (the transfer function outside the codestream) is
# made by running this with OpenEXR 3.5.1 installed (pip install OpenEXR==3.5.1) and
# ONLY=lj2k: those files are named lj2k351-*.
lj2k = "lj2k351" if C.__version__ in ("3.5.0", "3.5.1") else "lj2k"
write(f"{lj2k}-half-rgba", {"compression": C.LJ2K_COMPRESSION, "type": C.scanlineimage}, picture(H, W))
write(f"{lj2k}-layers", {"compression": C.LJ2K_COMPRESSION, "type": C.scanlineimage}, layered)
print("ok")
