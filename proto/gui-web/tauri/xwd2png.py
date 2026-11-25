"""xwd (ZPixmap, 24/32 bpp, from `xwd -root`) to PNG, stdlib only. usage: xwd2png.py in.xwd out.png"""
import struct, sys, zlib

d = open(sys.argv[1], 'rb').read()
hs = struct.unpack('>I', d[:4])[0]
f = struct.unpack('>25I', d[:100])
w, h = f[4], f[5]
byte_order = f[7]
bpp = f[11]
bpl = f[12]
ncolors = f[19]
off = hs + ncolors * 12
rows = []
for y in range(h):
    r = d[off + y * bpl: off + y * bpl + w * (bpp // 8)]
    out = bytearray(w * 3)
    step = bpp // 8
    for x in range(w):
        px = r[x * step:(x + 1) * step]
        if byte_order == 0:  # LSB first: B G R X
            b, g, rr = px[0], px[1], px[2]
        else:
            rr, g, b = px[-3], px[-2], px[-1]
        out[x * 3:x * 3 + 3] = bytes((rr, g, b))
    rows.append(b'\x00' + bytes(out))
def ch(t, dd):
    return struct.pack('>I', len(dd)) + t + dd + struct.pack('>I', zlib.crc32(t + dd) & 0xffffffff)
png = b'\x89PNG\r\n\x1a\n' + ch(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) + ch(b'IDAT', zlib.compress(b''.join(rows), 6)) + ch(b'IEND', b'')
open(sys.argv[2], 'wb').write(png)
print(w, h)
