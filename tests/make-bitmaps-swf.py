#!/usr/bin/env python3
"""A movie that keeps more pictures than the core's memory used to hold.

    make-bitmaps-swf.py <out.swf> [bitmaps] [frames]

Frame k places bitmap k, 2000 by 1600 pixels of one colour, at a depth of its
own, and nothing is ever removed, so every bitmap shown stays on the stage. A
bitmap on the stage keeps its pixels in the guest's memory, 12.8 MB each (the
size Corporate Climber ran out of memory asking for, 2026-10-23). The file
stays small: each bitmap is one colour, which zlib packs to a few kilobytes.
After the last bitmap the movie shows empty frames and stops on its last one.

It is what run-frontend.sh's engine:big-movie test plays.
"""
import struct
import sys
import zlib

WIDTH, HEIGHT = 2000, 1600


def rect(x0, x1, y0, y1):
    n = max(max(abs(v) for v in (x0, x1, y0, y1)).bit_length() + 1, 2)
    bits = format(n, "05b") + "".join(format(v & ((1 << n) - 1), "0%db" % n) for v in (x0, x1, y0, y1))
    bits += "0" * (-len(bits) % 8)
    return bytes(int(bits[i:i + 8], 2) for i in range(0, len(bits), 8))


def tag(code, body=b""):
    if len(body) < 63:
        return struct.pack("<H", (code << 6) | len(body)) + body
    return struct.pack("<HI", (code << 6) | 63, len(body)) + body


def bitmap(bitmap_id, rgb):
    """DefineBitsLossless2: 32-bit ARGB, opaque, one colour."""
    pixels = (bytes([255]) + bytes(rgb)) * (WIDTH * HEIGHT)
    return tag(36, struct.pack("<HBHH", bitmap_id, 5, WIDTH, HEIGHT) + zlib.compress(pixels, 9))


def place(depth, character_id):
    return tag(26, bytes([0x02]) + struct.pack("<HH", depth, character_id))  # has character


count = int(sys.argv[2]) if len(sys.argv) > 2 else 50
frames = int(sys.argv[3]) if len(sys.argv) > 3 else count + 10
stage = 4000  # 200 pixels; the bitmaps reach past it, which costs nothing
body = rect(0, stage, 0, stage) + bytes([0, 30]) + struct.pack("<H", frames)
body += tag(9, bytes([255, 255, 255]))
for k in range(frames):
    if k < count:
        rgb = ((k * 53) % 256, (k * 101) % 256, (k * 197) % 256)
        body += bitmap(k + 1, rgb) + place(k + 1, k + 1)
    if k == frames - 1:
        body += tag(12, b"\x07\x00")  # stop
    body += tag(1)
body += tag(0)
open(sys.argv[1], "wb").write(b"FWS" + bytes([8]) + struct.pack("<I", 8 + len(body)) + body)
