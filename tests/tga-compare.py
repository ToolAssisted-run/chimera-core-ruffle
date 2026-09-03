#!/usr/bin/env python3
"""Compare a frame the engine wrote (TGA, as chimera-run dumps it) with
ruffle's own expected PNG. Same tolerance argument as image-compare.py."""
import struct, sys, zlib

def read_png(p):
    d = open(p, 'rb').read()
    w, h = struct.unpack('>II', d[16:24])
    i, idat = 8, b''
    while i < len(d):
        ln = struct.unpack('>I', d[i:i+4])[0]
        if d[i+4:i+8] == b'IDAT': idat += d[i+8:i+8+ln]
        i += 12 + ln
    raw = zlib.decompress(idat); stride = w*4; out = []; prev = bytearray(stride); pos = 0
    for _ in range(h):
        f = raw[pos]; pos += 1
        line = bytearray(raw[pos:pos+stride]); pos += stride
        if f == 1:
            for x in range(4, stride): line[x] = (line[x]+line[x-4]) & 255
        elif f == 2:
            for x in range(stride): line[x] = (line[x]+prev[x]) & 255
        elif f == 3:
            for x in range(stride):
                a = line[x-4] if x >= 4 else 0
                line[x] = (line[x]+((a+prev[x]) >> 1)) & 255
        elif f == 4:
            for x in range(stride):
                a = line[x-4] if x >= 4 else 0
                c = prev[x-4] if x >= 4 else 0
                b = prev[x]
                pa, pb, pc = abs(b-c), abs(a-c), abs(a+b-2*c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[x] = (line[x]+pr) & 255
        prev = line; out.append(bytes(line))
    return w, h, b''.join(out)

def read_tga(p):
    d = open(p, 'rb').read()
    idlen = d[0]; w, h = struct.unpack('<HH', d[12:16]); desc = d[17]
    off = 18 + idlen
    px = d[off:off+w*h*4]
    rows = [px[y*w*4:(y+1)*w*4] for y in range(h)]
    if not (desc & 0x20): rows = rows[::-1]   # bottom-up origin
    return w, h, b''.join(rows)

pw, ph, png = read_png(sys.argv[1])
tw, th, tga = read_tga(sys.argv[2])
if (pw, ph) != (tw, th):
    sys.exit(f"SIZE MISMATCH expected {pw}x{ph} got {tw}x{th}")
tol = int(sys.argv[3]) if len(sys.argv) > 3 else 8
allowed = int(sys.argv[4]) if len(sys.argv) > 4 else 0
n = pw*ph; worst = 0; outliers = 0
for k in range(n):
    e = (png[k*4], png[k*4+1], png[k*4+2])
    g = (tga[k*4+2], tga[k*4+1], tga[k*4+0])   # TGA is BGRA
    dm = max(abs(e[i]-g[i]) for i in range(3))
    if dm > worst: worst = dm
    if dm > tol: outliers += 1
print(f"{pw}x{ph}: worst {worst}, {outliers} pixels beyond tolerance {tol} "
      f"({100.0*outliers/n:.3f}%), allowed {allowed}")
raise SystemExit(0 if outliers <= allowed else 1)
