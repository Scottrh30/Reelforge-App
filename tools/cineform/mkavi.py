# Wrap length-prefixed CineForm samples in an AVI: mkavi.py in.cfs W H out.avi
import struct, sys
src, w, h, out = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
d = open(src, 'rb').read(); frames = []; i = 0
while i < len(d):
    n = struct.unpack_from('<I', d, i)[0]; frames.append(d[i+4:i+4+n]); i += 4 + n
def chunk(tag, data):
    return tag + struct.pack('<I', len(data)) + data + (b'\0' if len(data) & 1 else b'')
def lst(tag, data):
    return b'LIST' + struct.pack('<I', len(data) + 4) + tag + data
mx = max(len(f) for f in frames)
avih = struct.pack('<IIIIIIIIIIIIII', 40000, 0, 0, 0x10, len(frames), 0, 1, mx, w, h, 0, 0, 0, 0)
strh = b'vids' + b'CFHD' + struct.pack('<IHHIIIIIIIIhhhh', 0, 0, 0, 0, 1, 25, 0, len(frames), mx, 0xFFFFFFFF, 0, 0, 0, w, h)
strf = struct.pack('<IiiHH4sIiiII', 40, w, h, 1, 24, b'CFHD', w * h * 3, 0, 0, 0, 0)
hdrl = lst(b'hdrl', chunk(b'avih', avih) + lst(b'strl', chunk(b'strh', strh) + chunk(b'strf', strf)))
movi = b''; idx = b''; off = 4
for f in frames:
    c = chunk(b'00dc', f); idx += b'00dc' + struct.pack('<III', 0x10, off, len(f)); movi += c; off += len(c)
body = b'AVI ' + hdrl + lst(b'movi', movi) + chunk(b'idx1', idx)
open(out, 'wb').write(b'RIFF' + struct.pack('<I', len(body)) + body)
