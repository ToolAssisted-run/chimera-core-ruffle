#!/usr/bin/env python3
"""Compare a frame the core drew against ruffle's own expected PNG.

ruffle's image tests carry their own tolerance because two GPUs never agree to
the last bit; we honour the same numbers rather than inventing our own. Prints
the worst channel difference and how many pixels exceeded the tolerance, and
exits non-zero when more than max_outliers did.
"""
import struct, zlib, sys

def read_png(p):
    d = open(p,'rb').read()
    w,h = struct.unpack('>II', d[16:24])
    i, idat, bd, ct = 8, b'', None, None
    while i < len(d):
        ln = struct.unpack('>I', d[i:i+4])[0]; typ = d[i+4:i+8]
        if typ == b'IHDR': bd, ct = d[i+16], d[i+17]
        if typ == b'IDAT': idat += d[i+8:i+8+ln]
        i += 12+ln
    if ct != 6 or bd != 8: raise SystemExit(f"unsupported png {ct=} {bd=}")
    raw = zlib.decompress(idat); stride = w*4
    out, prev, pos = [], bytearray(stride), 0
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
                line[x] = (line[x]+((a+prev[x])>>1)) & 255
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

def read_ppm(p):
    d = open(p,'rb').read()
    parts, i, vals = [], 2, []
    while len(vals) < 3:
        while d[i:i+1].isspace(): i += 1
        if d[i:i+1] == b'#':
            while d[i:i+1] != b'\n': i += 1
            continue
        j = i
        while not d[j:j+1].isspace(): j += 1
        vals.append(int(d[i:j])); i = j
    i += 1
    w, h, _ = vals
    return w, h, d[i:i+w*h*3]

pw, ph, png = read_png(sys.argv[1])
qw, qh, ppm = read_ppm(sys.argv[2])
if (pw, ph) != (qw, qh):
    print(f"SIZE MISMATCH expected {pw}x{ph} got {qw}x{qh}"); raise SystemExit(1)
tol = int(sys.argv[3]) if len(sys.argv) > 3 else 8
n = pw*ph
# drop the expected image's alpha so both sides are plain RGB triples
exp = bytearray(n*3)
exp[0::3] = png[0::4]; exp[1::3] = png[1::4]; exp[2::3] = png[2::4]
diffs = bytes(abs(a-b) for a, b in zip(exp, ppm))
worst = max(diffs) if diffs else 0
outliers = sum(1 for k in range(n)
               if diffs[k*3] > tol or diffs[k*3+1] > tol or diffs[k*3+2] > tol)
max_outliers = int(sys.argv[4]) if len(sys.argv) > 4 else 0
print(f"{pw}x{ph}: worst channel difference {worst}, {outliers} pixels beyond tolerance {tol} "
      f"({100.0*outliers/n:.3f}%), allowed {max_outliers}")
raise SystemExit(0 if outliers <= max_outliers else 1)
